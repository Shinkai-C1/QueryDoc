//! OCR 引擎装载：ONNX Runtime 初始化（load-dynamic）与四个模型会话加载。
//! 引擎随解析任务加载、任务结束即卸载释放内存（见 queue.rs run_parse_task），
//! 不常驻进程；资源目录解析覆盖 dev（CARGO_MANIFEST_DIR/resources）与
//! 可执行文件相对路径（macOS bundle Resources / Windows 同级 resources）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::session::{builder::GraphOptimizationLevel, Session};

use crate::error::AppError;

/// 环境变量：显式覆盖资源目录（测试/CI 用）。
pub const RESOURCES_ENV: &str = "QUERYDOC_RESOURCES_DIR";

const MODEL_DET: &str = "PP-OCRv6_small_det_onnx.onnx";
const MODEL_REC: &str = "PP-OCRv6_small_rec_onnx.onnx";
const MODEL_DOC_ORI: &str = "PP-LCNet_x1_0_doc_ori_onnx.onnx";
const MODEL_TEXTLINE_ORI: &str = "PP-LCNet_x0_25_textline_ori_onnx.onnx";
const REC_DICT: &str = "rec_dict.json";

/// ONNX Runtime dylib 路径（仅当前平台；Windows dll 由 Phase 8 CI 补齐）。
pub fn onnxruntime_dylib(resources: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        resources.join("onnxruntime/libonnxruntime.1.28.0.dylib")
    }
    #[cfg(target_os = "windows")]
    {
        resources.join("onnxruntime/onnxruntime.dll")
    }
}

/// pdfium 动态库路径。
pub fn pdfium_dylib(resources: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        resources.join("pdfium/libpdfium.dylib")
    }
    #[cfg(target_os = "windows")]
    {
        resources.join("pdfium/pdfium.dll")
    }
}

/// 资源目录候选（按优先级）。`override_dir` 来自环境变量 [`RESOURCES_ENV`]。
fn resource_candidates(override_dir: Option<PathBuf>) -> Vec<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(p) = override_dir {
        cands.push(p);
    }
    // dev：crate 清单目录（app/src-tauri/resources）
    cands.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // dev target 目录：app/src-tauri/target/{debug,release}/app → ../../resources
            cands.push(dir.join("../../resources"));
            // macOS bundle：Contents/MacOS/exe → Contents/Resources/resources
            cands.push(dir.join("../Resources/resources"));
            // Windows bundle（NSIS）：资源与 exe 同级的 resources/ 目录
            #[cfg(target_os = "windows")]
            cands.push(dir.join("resources"));
        }
    }
    cands
}

/// 解析资源目录：返回第一个含 `ocr-models/files` 标记的候选。
pub fn resolve_resources_dir_with(override_dir: Option<PathBuf>) -> Result<PathBuf, AppError> {
    let cands = resource_candidates(override_dir);
    first_valid(&cands).ok_or_else(|| {
        AppError::Internal(format!(
            "未找到 OCR 资源目录（尝试过: {}）",
            cands
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })
}

fn first_valid(cands: &[PathBuf]) -> Option<PathBuf> {
    cands
        .iter()
        .find(|c| c.join("ocr-models/files").is_dir())
        .cloned()
}

/// 解析资源目录（读取环境变量覆盖）。
pub fn resolve_resources_dir() -> Result<PathBuf, AppError> {
    resolve_resources_dir_with(std::env::var_os(RESOURCES_ENV).map(PathBuf::from))
}

fn ensure_ort(resources: &Path) -> Result<(), AppError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    let r = INIT.get_or_init(|| {
        let dylib = onnxruntime_dylib(resources);
        ort::init_from(&dylib)
            .map(|b| b.commit())
            .map(|_| ())
            .map_err(|e| format!("加载 ONNX Runtime 失败 {}: {e}", dylib.display()))
    });
    r.clone().map_err(AppError::Internal)
}

fn load_session(path: &Path) -> Result<Session, AppError> {
    Session::builder()
        .map_err(|e| AppError::Internal(format!("创建会话构建器失败: {e}")))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| AppError::Internal(format!("设置优化级别失败: {e}")))?
        .commit_from_file(path)
        .map_err(|e| AppError::Internal(format!("加载模型失败 {}: {e}", path.display())))
}

// ---------------------------------------------------------------------------
// 首启 SHA256 校验：引擎初始化前比对 models.toml 清单
// ---------------------------------------------------------------------------

struct ModelEntry {
    file: String,
    sha256: String,
}

/// 解析 models.toml 的固定结构：若干 `[[model]]` 块，取 `file` / `sha256` 键。
/// （与 app/scripts/download-models.mjs 的解析规则一致；本文件格式由我们维护。）
fn parse_models_toml(text: &str) -> Vec<ModelEntry> {
    let mut entries = Vec::new();
    let mut cur: Option<ModelEntry> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[model]]" {
            if let Some(e) = cur.take() {
                entries.push(e);
            }
            cur = Some(ModelEntry {
                file: String::new(),
                sha256: String::new(),
            });
            continue;
        }
        if let Some(e) = cur.as_mut() {
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"');
                match k.trim() {
                    "file" => e.file = v.to_string(),
                    "sha256" => e.sha256 = v.to_string(),
                    _ => {}
                }
            }
        }
    }
    if let Some(e) = cur.take() {
        entries.push(e);
    }
    entries.retain(|e| !e.file.is_empty());
    entries
}

fn sha256_file(path: &Path) -> Result<String, AppError> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// 对要加载的模型文件逐个校验 sha256（与 models.toml 清单比对）。
/// 不匹配 / 清单缺条目 / 文件缺失一律报错（错误信息含文件名），阻止引擎初始化。
fn verify_models(resources: &Path, required: &[&str]) -> Result<(), AppError> {
    let manifest_path = resources.join("ocr-models/models.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        AppError::Internal(format!("读取模型清单失败 {}: {e}", manifest_path.display()))
    })?;
    let entries = parse_models_toml(&text);
    let files_dir = resources.join("ocr-models/files");
    for &f in required {
        let entry = entries
            .iter()
            .find(|e| e.file == f)
            .ok_or_else(|| AppError::Internal(format!("模型清单 models.toml 缺少条目: {f}")))?;
        let path = files_dir.join(f);
        let actual = sha256_file(&path)
            .map_err(|e| AppError::Internal(format!("模型文件不可读 {f}: {e}")))?;
        if !actual.eq_ignore_ascii_case(&entry.sha256) {
            return Err(AppError::Internal(format!(
                "模型文件校验失败: {f}（sha256 不匹配，期望 {} 实际 {actual}；请重新运行 node app/scripts/download-models.mjs）",
                entry.sha256
            )));
        }
    }
    Ok(())
}

/// OCR 引擎：四个 ONNX 会话 + rec 字典（`Send + Sync`）。
/// 生命周期跟随解析任务：任务发起时加载，任务结束（成功/失败/停止）即析构释放，
/// 不常驻内存；下次解析重新加载（加载耗时见任务日志）。
pub struct OrtEngine {
    pub det: Session,
    pub rec: Session,
    pub doc_ori: Session,
    pub textline_ori: Session,
    /// rec 字符字典（18708 字；CTC 类 = blank + dict + 尾部空格类，共 18710）
    pub dict: Vec<String>,
}

impl OrtEngine {
    /// 按默认资源目录解析并加载。
    pub fn load() -> Result<Self, AppError> {
        Self::load_from(&resolve_resources_dir()?)
    }

    /// 从指定资源目录加载（测试可用绝对路径直指 resources）。
    pub fn load_from(resources: &Path) -> Result<Self, AppError> {
        ensure_ort(resources)?;
        // 首启校验（8.1）：清单哈希不匹配即阻止引擎初始化
        verify_models(
            resources,
            &[MODEL_DET, MODEL_REC, MODEL_DOC_ORI, MODEL_TEXTLINE_ORI],
        )?;
        let files = resources.join("ocr-models/files");
        let dict_text = std::fs::read_to_string(resources.join("ocr-models").join(REC_DICT))?;
        let dict: Vec<String> = serde_json::from_str(&dict_text)
            .map_err(|e| AppError::Internal(format!("解析 {REC_DICT} 失败: {e}")))?;
        Ok(OrtEngine {
            det: load_session(&files.join(MODEL_DET))?,
            rec: load_session(&files.join(MODEL_REC))?,
            doc_ori: load_session(&files.join(MODEL_DOC_ORI))?,
            textline_ori: load_session(&files.join(MODEL_TEXTLINE_ORI))?,
            dict,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_manifest_resources_in_dev() {
        // 无环境变量覆盖时，dev 下应命中 CARGO_MANIFEST_DIR/resources 分支
        let dir = resolve_resources_dir_with(None).unwrap();
        assert_eq!(
            dir.canonicalize().unwrap(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("resources")
                .canonicalize()
                .unwrap()
        );
        assert!(onnxruntime_dylib(&dir).exists(), "onnxruntime dylib 应存在");
        assert!(pdfium_dylib(&dir).exists(), "pdfium dylib 应存在");
        assert!(dir.join("ocr-models/rec_dict.json").exists());
    }

    #[test]
    fn explicit_override_wins() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("ocr-models/files")).unwrap();
        let dir = resolve_resources_dir_with(Some(tmp.path().to_path_buf())).unwrap();
        assert_eq!(dir, tmp.path());
    }

    #[test]
    fn invalid_override_falls_back_to_manifest() {
        // 覆盖路径无效时回落到后续候选（dev 下为 manifest resources）
        let tmp = tempfile::tempdir().unwrap();
        let dir = resolve_resources_dir_with(Some(tmp.path().join("nope"))).unwrap();
        assert_eq!(
            dir.canonicalize().unwrap(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("resources")
                .canonicalize()
                .unwrap()
        );
    }

    #[test]
    fn all_invalid_candidates_yield_none() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = vec![tmp.path().join("a"), tmp.path().join("b")];
        assert!(first_valid(&bad).is_none());
    }

    // ---- 首启 SHA256 校验（8.1） ----

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// 造一个含 2 个假模型与清单的 resources 目录。
    fn fake_resources(tamper: bool) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let files = tmp.path().join("ocr-models/files");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("a.onnx"), b"model-a").unwrap();
        std::fs::write(files.join("b.onnx"), b"model-b").unwrap();
        let manifest = format!(
            "[[model]]\nfile = \"a.onnx\"\nsha256 = \"{}\"\n\n[[model]]\nfile = \"b.onnx\"\nsha256 = \"{}\"\n",
            if tamper { "0".repeat(64) } else { sha256_hex(b"model-a") },
            sha256_hex(b"model-b")
        );
        std::fs::write(tmp.path().join("ocr-models/models.toml"), manifest).unwrap();
        tmp
    }

    #[test]
    fn verify_models_passes_with_correct_hashes() {
        let tmp = fake_resources(false);
        verify_models(tmp.path(), &["a.onnx", "b.onnx"]).unwrap();
    }

    #[test]
    fn verify_models_rejects_tampered_file_with_name() {
        let tmp = fake_resources(true);
        let e = verify_models(tmp.path(), &["a.onnx", "b.onnx"]).unwrap_err();
        assert!(e.to_string().contains("a.onnx"), "错误应含文件名: {e}");
        assert!(e.to_string().contains("校验失败"), "{e}");
    }

    #[test]
    fn verify_models_rejects_missing_manifest_entry() {
        let tmp = fake_resources(false);
        let e = verify_models(tmp.path(), &["c.onnx"]).unwrap_err();
        assert!(e.to_string().contains("c.onnx"), "{e}");
    }

    #[test]
    fn verify_models_passes_on_real_bundled_models() {
        // dev 环境真实 4 个随包模型应与 models.toml 清单一致（8.1 的直接证据）
        let dir = resolve_resources_dir_with(None).unwrap();
        verify_models(
            &dir,
            &[MODEL_DET, MODEL_REC, MODEL_DOC_ORI, MODEL_TEXTLINE_ORI],
        )
        .unwrap();
    }
}
