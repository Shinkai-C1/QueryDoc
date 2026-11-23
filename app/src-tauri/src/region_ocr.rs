//! 区域 OCR（内控测试框选识别，§3.6）：即用即载的独立 OrtEngine + Pdfium 实例，
//! 空闲 5 分钟延迟卸载（兜底上限 10 分钟内必然卸载）；与解析队列（queue.rs）内的
//! 引擎实例完全隔离，解析任务进行中也可框选 OCR（原则 4，queue.rs 不动）。
//!
//! 识别范围仅限用户框选矩形（不对整页识别）：渲染指定页（200 DPI，与解析
//! 管线一致）→ 裁剪矩形区域 → det + rec（复用 ocr/detect.rs、ocr/recognize.rs
//! 原语，只读调用）→ 坐标反映射回 0–1000 归一化 pdf 坐标。
//!
//! 生命周期由专属工作线程承载（Pdfium 非 Send，不能跨线程共享）：
//! 每次调用经 mpsc 投递任务，线程内即用即载；任务完成后 recv_timeout 等 5 分钟，
//! 无新任务则 drop 引擎与 pdfium（释放全部模型内存），其后无限等待下次调用。

use std::sync::mpsc;
use std::time::Duration;

use pdfium_render::prelude::*;
use serde::Serialize;

use crate::error::AppError;
use crate::ocr::executor::DEFAULT_DPI;
use crate::ocr::models::{self, OrtEngine};
use crate::ocr::{detect, recognize};

/// 行级 rec 置信度下限（与 executor::MIN_LINE_SCORE 一致）。
const MIN_LINE_SCORE: f32 = 0.75;

/// 空闲卸载倒计时（每次调用完成后重新起算；远小于 10 分钟兜底上限）。
const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);

/// 区域 OCR 结果：识别文本（按行 \n 连接）+ 各行矩形（0–1000 归一化）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OcrRegionResult {
    pub text: String,
    pub rects: Vec<[f64; 4]>,
}

struct OcrJob {
    pdf_bytes: Vec<u8>,
    page: i64,
    rect: [f64; 4],
    reply: mpsc::Sender<Result<OcrRegionResult, AppError>>,
}

/// 区域 OCR 管理器（AppState 持有；channel 串行化任务，天然并发去重）。
pub struct RegionOcr {
    tx: mpsc::Sender<OcrJob>,
}

impl Default for RegionOcr {
    fn default() -> Self {
        Self::new()
    }
}

impl RegionOcr {
    /// 启动专属工作线程（引擎/pdfium 常驻线程内，线程随 channel 关闭退出）。
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<OcrJob>();
        std::thread::Builder::new()
            .name("region-ocr".to_string())
            .spawn(move || worker_loop(rx))
            .expect("创建区域 OCR 工作线程失败");
        RegionOcr { tx }
    }

    /// 执行一次区域 OCR：即用即载（已加载则复用）。pdf 内容以字节传入（制度文件入库）。
    pub async fn run(
        &self,
        pdf_bytes: Vec<u8>,
        page: i64,
        rect: [f64; 4],
    ) -> Result<OcrRegionResult, AppError> {
        let (rtx, rrx) = mpsc::channel();
        self.tx
            .send(OcrJob {
                pdf_bytes,
                page,
                rect,
                reply: rtx,
            })
            .map_err(|_| AppError::Internal("区域 OCR 工作线程已退出".to_string()))?;
        // 在工作线程执行期间不阻塞 async runtime
        tokio::task::spawn_blocking(move || rrx.recv())
            .await
            .map_err(|e| AppError::Internal(format!("区域 OCR 任务失败: {e}")))?
            .map_err(|_| AppError::Internal("区域 OCR 工作线程无响应".to_string()))?
    }
}

/// 工作线程主循环：持有引擎与 pdfium；每次任务后 5 分钟空闲倒计时卸载。
fn worker_loop(rx: mpsc::Receiver<OcrJob>) {
    let mut engine: Option<OrtEngine> = None;
    let mut pdfium: Option<Pdfium> = None;
    loop {
        // 已加载：5 分钟内无新任务则卸载（卸载后转为无限等待；等价于
        //「空闲 5 分钟卸载 + 10 分钟兜底强制卸载」，因卸载点恒早于 10 分钟）。
        let job = if engine.is_some() {
            match rx.recv_timeout(IDLE_UNLOAD) {
                Ok(j) => j,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    engine = None;
                    pdfium = None;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(j) => j,
                Err(_) => break, // channel 关闭（应用退出）
            }
        };
        // 即用即载
        if engine.is_none() {
            match load_engine() {
                Ok((e, p)) => {
                    engine = Some(e);
                    pdfium = Some(p);
                }
                Err(err) => {
                    let _ = job.reply.send(Err(err));
                    continue;
                }
            }
        }
        let result = ocr_region_impl(
            engine.as_mut().expect("引擎已加载"),
            pdfium.as_ref().expect("pdfium 已加载"),
            &job.pdf_bytes,
            job.page,
            job.rect,
        );
        let _ = job.reply.send(result);
    }
    // 线程退出（应用退出）前兜底清理：drop 引擎与 pdfium 句柄
    drop(engine);
    drop(pdfium);
}

/// 加载独立引擎实例 + pdfium（与解析队列实例互不干扰）。
fn load_engine() -> Result<(OrtEngine, Pdfium), AppError> {
    let resources = models::resolve_resources_dir()?;
    let engine = OrtEngine::load_from(&resources)?;
    let pdfium = Pdfium::new(
        Pdfium::bind_to_library(models::pdfium_dylib(&resources))
            .map_err(|e| AppError::Internal(format!("绑定 pdfium 失败: {e}")))?,
    );
    Ok((engine, pdfium))
}

/// 0–1000 归一化矩形 → 页面像素裁剪框（clamp 到页内，最小 1px）。
fn norm_rect_to_px(rect: [f64; 4], w: u32, h: u32) -> (u32, u32, u32, u32) {
    let (wf, hf) = (w as f64, h as f64);
    let x0 = (rect[0] / 1000.0 * wf).floor().clamp(0.0, wf - 1.0) as u32;
    let y0 = (rect[1] / 1000.0 * hf).floor().clamp(0.0, hf - 1.0) as u32;
    let x1 = (rect[2] / 1000.0 * wf).ceil().clamp(x0 as f64 + 1.0, wf) as u32;
    let y1 = (rect[3] / 1000.0 * hf).ceil().clamp(y0 as f64 + 1.0, hf) as u32;
    (x0, y0, x1, y1)
}

/// 页面像素 bbox → 0–1000 归一化。
fn px_to_norm(b: [f32; 4], w: u32, h: u32) -> [f64; 4] {
    [
        (b[0] as f64 / w as f64 * 1000.0).clamp(0.0, 1000.0),
        (b[1] as f64 / h as f64 * 1000.0).clamp(0.0, 1000.0),
        (b[2] as f64 / w as f64 * 1000.0).clamp(0.0, 1000.0),
        (b[3] as f64 / h as f64 * 1000.0).clamp(0.0, 1000.0),
    ]
}

/// 区域识别实现：字节加载 pdf → 渲染指定页 → 裁剪框选区域 → det → 逐行 rec → 坐标反映射。
fn ocr_region_impl(
    engine: &mut OrtEngine,
    pdfium: &Pdfium,
    pdf_bytes: &[u8],
    page: i64,
    rect: [f64; 4],
) -> Result<OcrRegionResult, AppError> {
    let doc = pdfium
        .load_pdf_from_byte_slice(pdf_bytes, None)
        .map_err(|e| AppError::Internal(format!("打开 PDF 失败: {e}")))?;
    let n = doc.pages().len() as i64;
    if !(1..=n).contains(&page) {
        return Err(AppError::InvalidInput(format!(
            "页码超出范围: {page}（共 {n} 页）"
        )));
    }
    let pdf_page = doc
        .pages()
        .get((page - 1) as u16)
        .map_err(|e| AppError::Internal(format!("读取第 {page} 页失败: {e}")))?;
    let bitmap = pdf_page
        .render_with_config(&PdfRenderConfig::new().scale_page_by_factor(DEFAULT_DPI as f32 / 72.0))
        .map_err(|e| AppError::Internal(format!("渲染第 {page} 页失败: {e}")))?;
    let img = bitmap.as_image().to_rgb8();
    let (w, h) = img.dimensions();

    // 仅框选区域裁剪（padding 少许放宽，避免边缘字符被切断）
    let (mut x0, mut y0, mut x1, mut y1) = norm_rect_to_px(rect, w, h);
    let pad = 4u32;
    x0 = x0.saturating_sub(pad);
    y0 = y0.saturating_sub(pad);
    x1 = (x1 + pad).min(w);
    y1 = (y1 + pad).min(h);
    let crop = image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image();

    // det（在裁剪图上）
    let (input, _rw, _rh) = detect::preprocess(&crop);
    let (prob, mw, mh) = detect::infer(&mut engine.det, input)?;
    let quads = detect::postprocess(
        &prob,
        mw,
        mh,
        crop.width(),
        crop.height(),
        &detect::DbParams::default(),
    );

    // 逐行 rec（行框取轴对齐包围盒 + padding；区域假定正立，不做 doc_ori/deskew）
    let (cw, ch) = crop.dimensions();
    let mut texts: Vec<String> = Vec::new();
    let mut rects: Vec<[f64; 4]> = Vec::new();
    for (quad, _det_score) in &quads {
        let bb = quad.bbox();
        let line_pad = (bb[3] - bb[1]).mul_add(0.12, 2.0);
        let lx0 = (bb[0] - line_pad).max(0.0) as u32;
        let ly0 = (bb[1] - line_pad).max(0.0) as u32;
        let lx1 = ((bb[2] + line_pad) as u32).min(cw);
        let ly1 = ((bb[3] + line_pad) as u32).min(ch);
        if lx1 <= lx0 || ly1 <= ly0 {
            continue;
        }
        let line_img = image::imageops::crop_imm(&crop, lx0, ly0, lx1 - lx0, ly1 - ly0).to_image();
        let (rec_in, resized_w) = recognize::preprocess(&line_img);
        let (logits, t_steps, n_cls) = recognize::infer(&mut engine.rec, rec_in)?;
        let rr = recognize::decode(&logits, t_steps, n_cls, &engine.dict, resized_w);
        if rr.text.trim().is_empty() || rr.score < MIN_LINE_SCORE {
            continue; // 空行/低置信行剔除（印章/噪声区域）
        }
        // 行框（crop 坐标，取轴对齐包围盒）→ 页面像素 → 0–1000 归一化
        let page_bbox = [
            bb[0] + x0 as f32,
            bb[1] + y0 as f32,
            bb[2] + x0 as f32,
            bb[3] + y0 as f32,
        ];
        texts.push(rr.text);
        rects.push(px_to_norm(page_bbox, w, h));
    }

    Ok(OcrRegionResult {
        text: texts.join("\n"),
        rects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_rect_to_px_clamps_and_expands() {
        // 1000x1000 页面：直接映射
        assert_eq!(
            norm_rect_to_px([100.0, 200.0, 300.0, 400.0], 1000, 1000),
            (100, 200, 300, 400)
        );
        // 出界 clamp 到页内
        assert_eq!(
            norm_rect_to_px([0.0, 0.0, 1000.0, 1000.0], 2000, 1000),
            (0, 0, 2000, 1000)
        );
        // 非整数边界向外取整（1000px 页面上归一化值即像素值）
        assert_eq!(
            norm_rect_to_px([100.4, 200.4, 300.6, 400.6], 1000, 1000),
            (100, 200, 301, 401)
        );
    }

    #[test]
    fn px_to_norm_roundtrip() {
        let n = px_to_norm([100.0, 200.0, 300.0, 400.0], 1000, 2000);
        assert_eq!(n, [100.0, 100.0, 300.0, 200.0]);
        // 出界 clamp 到 1000
        let c = px_to_norm([-5.0, 0.0, 1200.0, 2000.0], 1000, 2000);
        assert_eq!(c, [0.0, 0.0, 1000.0, 1000.0]);
    }

    #[test]
    fn worker_channel_roundtrip_without_models() {
        // 工作线程可启动；无效 pdf 字节返回错误而非 panic（引擎加载在 dev 资源下真实发生）
        let ocr = RegionOcr::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let r = rt.block_on(ocr.run(b"not-a-pdf".to_vec(), 1, [0.0, 0.0, 10.0, 10.0]));
        assert!(r.is_err(), "无效 pdf 应报错");
    }
}
