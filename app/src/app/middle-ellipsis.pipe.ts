import { Pipe, PipeTransform } from "@angular/core";

export interface MidEllipsisParts {
  head: string;
  tail: string;
}

/**
 * 文件名中间省略：超长时拆成 head/tail 两段，由模板用 flex +
 * head 端 ellipsis 实现「前缀…后缀.pdf」式截断（防文字超出列宽）。
 * 短名不拆（tail 为空，head 即全名，截断退化为末尾省略号）。
 */
@Pipe({ name: "midEllipsis", pure: true })
export class MidEllipsisPipe implements PipeTransform {
  /** 尾部保留字符数（含扩展名）；短于该阈值不拆分。 */
  private static readonly TAIL = 8;
  private static readonly MIN_SPLIT = 18;

  transform(name: string | null | undefined): MidEllipsisParts {
    const s = name ?? "";
    if (s.length <= MidEllipsisPipe.MIN_SPLIT) {
      return { head: s, tail: "" };
    }
    return {
      head: s.slice(0, -MidEllipsisPipe.TAIL),
      tail: s.slice(-MidEllipsisPipe.TAIL),
    };
  }
}
