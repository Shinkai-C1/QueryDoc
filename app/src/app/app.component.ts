import {
  ChangeDetectionStrategy,
  Component,
  CUSTOM_ELEMENTS_SCHEMA,
  HostListener,
  OnDestroy,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import { ClarityIcons } from "@cds/core/icon";
import { cogIcon } from "@cds/core/icon/shapes/cog.js";
import "@cds/core/icon/register.js";
import { RouterLink, RouterLinkActive, RouterOutlet } from "@angular/router";
import { AppStateService } from "./app-state.service";
import { SettingsModalComponent } from "./settings-modal.component";

// header 设置按钮齿轮图标
ClarityIcons.addIcons(cogIcon);

@Component({
  selector: "app-root",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [RouterOutlet, RouterLink, RouterLinkActive, SettingsModalComponent],
  templateUrl: "./app.component.html",
  styleUrl: "./app.component.css",
})
export class AppComponent implements OnInit, OnDestroy {
  protected readonly state = inject(AppStateService);
  readonly settingsOpen = signal(false);

  /** 窗口整体缩放比例（Ctrl/Cmd +「+」「-」「0」，持久化到 localStorage）。 */
  readonly zoomFactor = signal(1);

  private static readonly ZOOM_KEY = "querydoc-zoom";

  async ngOnInit(): Promise<void> {
    const saved = Number(localStorage.getItem(AppComponent.ZOOM_KEY));
    if (saved >= 0.5 && saved <= 2) {
      this.applyZoom(saved);
    }
    await this.state.init();
  }

  ngOnDestroy(): void {
    this.state.destroy();
  }

  /** 浏览器式整体缩放：所有要素等比例变化，不影响功能。 */
  @HostListener("document:keydown", ["$event"])
  onZoomKeys(e: KeyboardEvent): void {
    if (!e.ctrlKey && !e.metaKey) return;
    if (e.key === "=" || e.key === "+") {
      e.preventDefault();
      this.adjustZoom(0.1);
    } else if (e.key === "-") {
      e.preventDefault();
      this.adjustZoom(-0.1);
    } else if (e.key === "0") {
      e.preventDefault();
      this.setZoom(1);
    }
  }

  private adjustZoom(delta: number): void {
    this.setZoom(this.zoomFactor() + delta);
  }

  private setZoom(z: number): void {
    const f = Math.min(2, Math.max(0.5, Math.round(z * 10) / 10));
    this.applyZoom(f);
    localStorage.setItem(AppComponent.ZOOM_KEY, String(f));
  }

  private applyZoom(f: number): void {
    this.zoomFactor.set(f);
    document.body.style.zoom = String(f);
  }
}
