import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { closeOverlays } from "./sidebar";
import { t } from "./i18n";

interface ConfigSummary {
  format_version: number;
  hosts: number;
  groups: number;
  keys: number;
  includes_credentials: boolean;
}

/** 备份 / 迁移面板。口令只用于本次本机加解密，不写日志、不交给 MCP。 */
export class ConfigPanel {
  private root = document.getElementById("config-panel") as HTMLElement;
  private exportSecrets = document.getElementById("cfg-export-secrets") as HTMLInputElement;
  private exportPassRow = document.getElementById("cfg-export-pass-row") as HTMLElement;
  private exportPass = document.getElementById("cfg-export-pass") as HTMLInputElement;
  private exportPass2 = document.getElementById("cfg-export-pass2") as HTMLInputElement;
  private exportStatus = document.getElementById("cfg-export-status") as HTMLElement;
  private importFile = document.getElementById("cfg-import-file") as HTMLElement;
  private importMeta = document.getElementById("cfg-import-meta") as HTMLElement;
  private importPassRow = document.getElementById("cfg-import-pass-row") as HTMLElement;
  private importPass = document.getElementById("cfg-import-pass") as HTMLInputElement;
  private importStatus = document.getElementById("cfg-import-status") as HTMLElement;
  private importButton = document.getElementById("cfg-import-btn") as HTMLButtonElement;
  private path = "";
  private summary: ConfigSummary | null = null;

  constructor(private onChanged: () => void) {
    (document.getElementById("config-close") as HTMLElement).addEventListener("click", () => {
      this.root.hidden = true;
    });
    (document.getElementById("cfg-export-btn") as HTMLElement).addEventListener("click", () => {
      void this.export();
    });
    (document.getElementById("cfg-import-choose") as HTMLElement).addEventListener("click", () => {
      void this.chooseImport();
    });
    this.importButton.addEventListener("click", () => void this.import());
    this.exportSecrets.addEventListener("change", () => this.syncSecretRow());
    this.importPass.addEventListener("input", () => this.syncImportButton());
    window.addEventListener("dq:locale", () => this.renderSummary());
  }

  async open(): Promise<void> {
    closeOverlays("config-panel");
    this.root.hidden = false;
    this.exportStatus.textContent = "";
    this.importStatus.textContent = "";
    this.syncSecretRow();
  }

  private syncSecretRow(): void {
    this.exportPassRow.hidden = !this.exportSecrets.checked;
  }

  private async export(): Promise<void> {
    const includeCredentials = this.exportSecrets.checked;
    const passphrase = includeCredentials ? this.exportPass.value : null;
    if (includeCredentials && !passphrase) {
      this.exportStatus.textContent = t("请输入口令");
      return;
    }
    if (includeCredentials && passphrase !== this.exportPass2.value) {
      this.exportStatus.textContent = t("两次输入的口令不一致");
      return;
    }

    const date = new Date().toISOString().slice(0, 10);
    const path = await saveFileDialog({
      defaultPath: `${t("代敲配置")}-${date}.dqconfig`,
      filters: [{ name: t("代敲配置"), extensions: ["dqconfig"] }],
    });
    if (!path) return;

    this.exportStatus.textContent = t("正在导出…");
    try {
      const result = await invoke<string>("export_config", {
        path,
        includeCredentials,
        passphrase,
      });
      this.exportStatus.textContent = result;
      this.exportPass.value = "";
      this.exportPass2.value = "";
    } catch (err) {
      this.exportStatus.textContent = `${err}`;
    }
  }

  private async chooseImport(): Promise<void> {
    const picked = await openFileDialog({
      multiple: false,
      filters: [{ name: "代敲配置", extensions: ["dqconfig"] }],
    });
    if (!picked || Array.isArray(picked)) return;
    // 新文件选中后，旧文件的元数据和口令立即作废。
    // 在 inspect_config 返回前，绝不允许用新路径+旧摘要提交导入。
    this.path = picked;
    this.summary = null;
    this.importPass.value = "";
    this.importButton.disabled = true;
    this.importFile.textContent = picked;
    this.importMeta.textContent = t("检测中…");
    this.importStatus.textContent = "";
    try {
      this.summary = await invoke<ConfigSummary>("inspect_config", { path: picked });
      this.renderSummary();
    } catch (err) {
      this.path = "";
      this.summary = null;
      this.importMeta.textContent = `${err}`;
      this.importButton.disabled = true;
    }
  }

  private renderSummary(): void {
    const summary = this.summary;
    if (!summary) return;
    this.importMeta.textContent = t("已选择 {count} 台主机、{groups} 个分组、{keys} 把密钥", {
      count: summary.hosts,
      groups: summary.groups,
      keys: summary.keys,
    });
    this.importPassRow.hidden = !summary.includes_credentials;
    this.importPass.placeholder = summary.includes_credentials
      ? t("这个包包含凭据，导入需要口令")
      : t("这个包不含凭据");
    this.syncImportButton();
  }

  private syncImportButton(): void {
    this.importButton.disabled =
      !this.path || !this.summary || (this.summary.includes_credentials && !this.importPass.value);
  }

  private async import(): Promise<void> {
    if (!this.path || !this.summary) return;
    if (this.summary.includes_credentials && !this.importPass.value) {
      // 不依赖按钮 disabled：函数自身也拒绝空口令，防止异步状态或程序化点击绕过。
      this.importStatus.textContent = t("请输入口令");
      this.syncImportButton();
      return;
    }
    this.importStatus.textContent = t("正在导入…");
    try {
      const result = await invoke<string>("import_config", {
        path: this.path,
        passphrase: this.summary.includes_credentials ? this.importPass.value : null,
      });
      this.importStatus.textContent = result;
      this.importPass.value = "";
      // 成功后整单作废，避免用空口令或过期摘要重复提交。
      this.path = "";
      this.summary = null;
      this.importFile.textContent = "";
      this.importMeta.textContent = "";
      this.syncImportButton();
      this.onChanged();
    } catch (err) {
      this.importStatus.textContent = `${err}`;
    }
  }
}
