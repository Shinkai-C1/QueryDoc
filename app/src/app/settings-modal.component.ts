import {
  CUSTOM_ELEMENTS_SCHEMA,
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { ClarityIcons } from "@cds/core/icon";
import { refreshIcon } from "@cds/core/icon/shapes/refresh.js";
import "@cds/core/icon/register.js";
import { AppStateService } from "./app-state.service";
import { BackendService } from "./backend.service";
import { ProviderSettings } from "./backend.models";
import { ModalComponent } from "./modal.component";
import { NoticeComponent, NoticeType } from "./notice.component";

ClarityIcons.addIcons(refreshIcon);

interface NoticeMsg {
  type: NoticeType;
  text: string;
}

const PROVIDER_OLLAMA = "ollama";
const PROVIDER_KIMI = "kimi";

function blankProviderSettings(): ProviderSettings {
  return { base_url: "", api_key: "", model: "", timeout: 300 };
}

/**
 * 设置 modal：provider 下拉选择，两个 provider 各自的完整配置分存分显；
 * 模型为下拉选项，右侧刷新按钮按当前表单 api 地址/key 获取模型清单并自动选中第一个。
 */
@Component({
  selector: "app-settings-modal",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [FormsModule, ModalComponent, NoticeComponent],
  template: `
    <app-modal
      [open]="open()"
      title="设置"
      size="lg"
      [closable]="true"
      (closed)="closed.emit()"
    >
      @if (loading()) {
        <p>设置加载中…</p>
      } @else {
        @if (notice(); as n) {
          <app-notice [type]="n.type" (closed)="notice.set(null)">{{ n.text }}</app-notice>
        }
        @if (validationError(); as err) {
          <app-notice type="warning" [closable]="false">{{ err }}</app-notice>
        }

        <div class="clr-form-control">
          <label class="clr-control-label" for="set-provider">LLM 服务</label>
          <div class="clr-control-container">
            <div class="clr-select-wrapper">
              <select
                id="set-provider"
                class="clr-select"
                [ngModel]="provider()"
                (ngModelChange)="provider.set($event)"
              >
                <option value="ollama">本地 Ollama</option>
                <option value="kimi">KIMI-API</option>
              </select>
            </div>
          </div>
        </div>

        <div class="clr-form-control">
          <label class="clr-control-label" for="set-base-url">API 地址</label>
          <div class="clr-control-container">
            <div class="clr-input-wrapper">
              <input
                id="set-base-url"
                type="text"
                class="clr-input"
                [placeholder]="provider() === 'ollama' ? 'http://localhost:11434' : 'https://api.moonshot.cn/v1'"
                [ngModel]="draft().base_url"
                (ngModelChange)="patchDraft({ base_url: $event })"
              />
            </div>
          </div>
        </div>

        <div class="clr-form-control">
          <label class="clr-control-label" for="set-api-key">API Key</label>
          <div class="clr-control-container">
            <div class="clr-input-wrapper">
              <input
                id="set-api-key"
                type="password"
                class="clr-input"
                [placeholder]="provider() === 'ollama' ? '本地 Ollama 无需填写' : 'sk-…（必填）'"
                [disabled]="provider() === 'ollama'"
                [ngModel]="draft().api_key"
                (ngModelChange)="patchDraft({ api_key: $event })"
              />
            </div>
          </div>
        </div>

        <div class="clr-form-control">
          <label class="clr-control-label" for="set-model">模型</label>
          <div class="clr-control-container model-row">
            <div class="clr-select-wrapper model-select">
              <select
                id="set-model"
                class="clr-select"
                [ngModel]="draft().model"
                (ngModelChange)="patchDraft({ model: $event })"
              >
                @for (m of modelOptions(); track m) {
                  <option [value]="m">{{ m }}</option>
                }
                @if (modelOptions().length === 0) {
                  <option value="" disabled>（点击右侧刷新获取模型清单）</option>
                }
              </select>
            </div>
            <button
              type="button"
              class="btn btn-sm btn-outline refresh-btn"
              [disabled]="fetchingModels()"
              (click)="refreshModels()"
            >
              <cds-icon shape="refresh" size="sm"></cds-icon>
              {{ fetchingModels() ? "获取中…" : "连接获取模型清单" }}
            </button>
          </div>
          <span class="field-hint">可下拉选择模型</span>
        </div>

        <div class="clr-form-control number-row">
          <div class="number-item">
            <label class="clr-control-label" for="set-timeout">超时（秒）</label>
            <div class="clr-control-container">
              <div class="clr-input-wrapper">
                <input
                  id="set-timeout"
                  type="number"
                  class="clr-input"
                  min="10"
                  max="3600"
                  [ngModel]="draft().timeout"
                  (ngModelChange)="patchDraft({ timeout: asNumber($event) })"
                />
              </div>
            </div>
          </div>
        </div>
      }

      <div modalFooter>
        <span class="footer-spacer"></span>
        <button
          type="button"
          class="btn btn-sm btn-primary"
          [disabled]="saving() || loading() || !!validationError() || !dirty()"
          (click)="save()"
        >
          {{ saving() ? "保存中…" : dirty() ? "保存" : "已保存" }}
        </button>
        @if (dirty()) {
          <span class="test-hint">有未保存修改</span>
        }
      </div>
    </app-modal>
  `,
  styles: `
    .clr-form-control {
      margin-top: 0.35rem;
    }
    .clr-form-control:first-of-type {
      margin-top: 0;
    }
    .model-row {
      display: flex;
      gap: 0.4rem;
      align-items: flex-start;
    }
    .model-select {
      flex: 1;
      min-width: 0;
    }
    .refresh-btn {
      flex: none;
      display: inline-flex;
      align-items: center;
      gap: 0.2rem;
      white-space: nowrap;
    }
    .number-row {
      display: flex;
      gap: 0.8rem;
    }
    .number-item {
      flex: 1;
    }
    .field-hint,
    .test-hint {
      font-size: 0.55rem;
      color: var(--cds-alias-typography-color-400, #666);
    }
    .test-hint {
      align-self: center;
    }
    .footer-spacer {
      flex: 1;
    }
  `,
})
export class SettingsModalComponent {
  readonly open = input.required<boolean>();
  readonly closed = output<void>();

  private readonly backend = inject(BackendService);
  private readonly state = inject(AppStateService);

  readonly loading = signal(false);
  readonly saving = signal(false);
  readonly fetchingModels = signal(false);
  readonly notice = signal<NoticeMsg | null>(null);

  /** 当前展示的 provider（下拉选择）。 */
  readonly provider = signal(PROVIDER_OLLAMA);
  /** 两个 provider 各自的草稿（切换不丢未保存编辑）。 */
  private readonly drafts = signal<Record<string, ProviderSettings>>({
    [PROVIDER_OLLAMA]: blankProviderSettings(),
    [PROVIDER_KIMI]: blankProviderSettings(),
  });
  /** 当前 provider 的模型清单选项（含已保存模型名）。 */
  private readonly fetchedModels = signal<string[]>([]);

  /** 当前 provider 的草稿。 */
  readonly draft = computed(() => this.drafts()[this.provider()]);

  /** 模型下拉选项：已保存模型置顶（若不在清单中）+ 已获取清单。 */
  readonly modelOptions = computed(() => {
    const cur = this.draft().model;
    const list = this.fetchedModels();
    if (cur && !list.includes(cur)) return [cur, ...list];
    return list.length > 0 ? list : cur ? [cur] : [];
  });

  /** 最近一次从后端读到的快照（按 provider 分别比较，dirty 判定用） */
  private savedSnapshot = signal("");
  /** 已保存的当前 provider（单纯切换服务商也构成"修改"，可保存） */
  private savedActive = signal("");

  readonly validationError = computed(() => {
    const d = this.draft();
    const base = d.base_url.trim();
    if (!base.startsWith("http://") && !base.startsWith("https://")) {
      return "API 地址需以 http(s):// 开头";
    }
    if (this.provider() === PROVIDER_KIMI && !d.api_key.trim()) {
      return "KIMI-API 需要填写 API Key";
    }
    const t = d.timeout;
    if (!Number.isInteger(t) || t < 10 || t > 3600) {
      return "超时需为 10–3600 的整数（秒）";
    }
    return null;
  });

  /** 当前 provider 的草稿相对其已保存配置有修改，或切换了服务商，即可保存。 */
  readonly dirty = computed(() => {
    if (this.savedSnapshot() === "") return false;
    if (this.provider() !== this.savedActive()) return true; // 切换服务商
    return this.snapshotOf(this.provider()) !== this.savedSnapshotOf(this.provider());
  });

  constructor() {
    effect(() => {
      if (this.open()) {
        void this.load();
      }
    });
  }

  asNumber(v: unknown): number {
    return Number(v);
  }

  patchDraft(patch: Partial<ProviderSettings>): void {
    const p = this.provider();
    this.drafts.update((d) => ({ ...d, [p]: { ...d[p], ...patch } }));
  }

  private snapshotOf(p: string): string {
    return JSON.stringify(this.drafts()[p]);
  }

  private savedSnapshotOf(p: string): string {
    return JSON.parse(this.savedSnapshot() || "{}")[p] ?? "";
  }

  private async load(): Promise<void> {
    this.loading.set(true);
    this.notice.set(null);
    try {
      const v = await this.backend.getSettings();
      this.provider.set(v.active_provider);
      this.savedActive.set(v.active_provider);
      this.drafts.set({
        [PROVIDER_OLLAMA]: { ...v.ollama },
        [PROVIDER_KIMI]: { ...v.kimi },
      });
      this.fetchedModels.set([]);
      this.savedSnapshot.set(
        JSON.stringify({
          [PROVIDER_OLLAMA]: JSON.stringify(v.ollama),
          [PROVIDER_KIMI]: JSON.stringify(v.kimi),
        }),
      );
    } catch (e) {
      this.notice.set({ type: "error", text: `读取设置失败: ${e}` });
    } finally {
      this.loading.set(false);
    }
  }

  /** 刷新模型清单：按当前表单草稿的 api 地址/key 请求，成功后填充下拉选项，
   *  不自动选中——由用户自行选择确定具体模型后再保存。 */
  async refreshModels(): Promise<void> {
    const d = this.draft();
    this.fetchingModels.set(true);
    this.notice.set(null);
    try {
      const r = await this.backend.fetchModels(this.provider(), d.base_url.trim(), d.api_key);
      if (r.ok && r.models.length > 0) {
        this.fetchedModels.set(r.models);
        this.notice.set({ type: "success", text: r.message });
      } else {
        this.notice.set({
          type: "error",
          text: r.ok ? "连接成功，但返回的模型清单为空" : r.message,
        });
      }
    } catch (e) {
      this.notice.set({ type: "error", text: `获取模型清单失败: ${e}` });
    } finally {
      this.fetchingModels.set(false);
    }
  }

  /** 保存当前展示的 provider 配置（保存即设为当前使用 provider）。 */
  async save(): Promise<void> {
    if (this.validationError()) return;
    const d = this.draft();
    this.saving.set(true);
    this.notice.set(null);
    try {
      const v = await this.backend.saveSettings({
        provider: this.provider(),
        base_url: d.base_url.trim(),
        api_key: this.provider() === PROVIDER_OLLAMA ? "" : d.api_key,
        model: d.model.trim(),
        timeout: d.timeout,
      });
      this.drafts.set({
        [PROVIDER_OLLAMA]: { ...v.ollama },
        [PROVIDER_KIMI]: { ...v.kimi },
      });
      this.savedSnapshot.set(
        JSON.stringify({
          [PROVIDER_OLLAMA]: JSON.stringify(v.ollama),
          [PROVIDER_KIMI]: JSON.stringify(v.kimi),
        }),
      );
      this.savedActive.set(v.active_provider);
      await this.state.refreshSettings();
      this.notice.set({ type: "success", text: "设置已保存" });
    } catch (e) {
      this.notice.set({ type: "error", text: `保存失败: ${e}` });
    } finally {
      this.saving.set(false);
    }
  }
}
