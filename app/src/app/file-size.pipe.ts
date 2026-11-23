import { Pipe, PipeTransform } from "@angular/core";

/** 文件大小人性化显示（B/KB/MB）。 */
@Pipe({ name: "fileSize", pure: true })
export class FileSizePipe implements PipeTransform {
  transform(bytes: number): string {
    if (!Number.isFinite(bytes) || bytes < 0) return "-";
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  }
}
