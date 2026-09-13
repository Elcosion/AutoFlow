import { useEffect, useMemo, useRef, useState } from "react";
import {
  deleteAsset,
  importAsset,
  readAsset,
  renameAsset,
} from "../lib/tauri";
import type { AutomationAsset } from "../types/config";

type AssetManagerProps = {
  assets: AutomationAsset[];
  refresh: () => Promise<unknown>;
  onError: (message: string) => void;
};

function toErrorMessage(reason: unknown): string {
  if (typeof reason === "string") return reason;
  if (reason && typeof reason === "object" && "message" in reason) {
    const message = (reason as { message?: unknown }).message;
    if (typeof message === "string") return message;
  }
  return "图像资源操作失败，请重试";
}

function mimeType(fileName: string): string {
  return fileName.toLowerCase().endsWith(".png") ? "image/png" : "image/jpeg";
}

export function isSupportedAssetFileName(fileName: string): boolean {
  const lower = fileName.toLowerCase();
  return lower.endsWith(".png") || lower.endsWith(".jpg") || lower.endsWith(".jpeg");
}

export function AssetManager({ assets, refresh, onError }: AssetManagerProps) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [previews, setPreviews] = useState<Record<string, string>>({});
  const [missing, setMissing] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    const urls: string[] = [];
    void Promise.all(
      assets.map(async (asset) => {
        try {
          const bytes = await readAsset(asset.id);
          if (!active) return;
          const url = URL.createObjectURL(
            new Blob([bytes], { type: mimeType(asset.fileName) }),
          );
          urls.push(url);
          setPreviews((current) => ({ ...current, [asset.id]: url }));
          setMissing((current) => {
            const next = new Set(current);
            next.delete(asset.id);
            return next;
          });
        } catch {
          if (active) setMissing((current) => new Set(current).add(asset.id));
        }
      }),
    );
    return () => {
      active = false;
      urls.forEach((url) => URL.revokeObjectURL(url));
    };
  }, [assets]);

  const assetCountLabel = useMemo(() => `${assets.length} 个资源`, [assets.length]);

  const importSelected = async (file: File) => {
    if (!isSupportedAssetFileName(file.name)) {
      onError("只支持导入 PNG 或 JPEG 图像");
      return;
    }
    setBusy(true);
    try {
      const name = file.name.replace(/\.(png|jpe?g)$/i, "") || "新图像资源";
      await importAsset(name, file.name, new Uint8Array(await file.arrayBuffer()));
      await refresh();
      setNotice("图像资源已导入");
    } catch (reason) {
      onError(toErrorMessage(reason));
    } finally {
      setBusy(false);
      if (inputRef.current) inputRef.current.value = "";
    }
  };

  const rename = async (asset: AutomationAsset) => {
    const name = window.prompt("资源名称", asset.name)?.trim();
    if (!name || name === asset.name) return;
    setBusy(true);
    try {
      await renameAsset(asset.id, name);
      await refresh();
      setNotice("资源名称已更新");
    } catch (reason) {
      onError(toErrorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (asset: AutomationAsset) => {
    if (!window.confirm(`确认删除资源“${asset.name}”吗？引用它的 Rhai 脚本也会失效。`)) return;
    setBusy(true);
    try {
      await deleteAsset(asset.id, true);
      await refresh();
      setNotice("图像资源已删除");
    } catch (reason) {
      onError(toErrorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const copyId = async (asset: AutomationAsset) => {
    try {
      await navigator.clipboard.writeText(asset.id);
      setNotice("资源 ID 已复制");
    } catch {
      onError("复制资源 ID 失败，请手动选择复制");
    }
  };

  return (
    <section className="asset-manager">
      <div className="asset-manager-heading">
        <div>
          <strong>图像资源</strong>
          <span>托管在 AutoFlow 资源目录中，Rhai 只能通过资源 ID 使用。</span>
        </div>
        <div className="asset-manager-actions">
          <span>{assetCountLabel}</span>
          <button
            className="button button-primary"
            disabled={busy}
            onClick={() => inputRef.current?.click()}
            type="button"
          >
            导入 PNG/JPEG
          </button>
          <input
            accept=".png,.jpg,.jpeg,image/png,image/jpeg"
            hidden
            onChange={(event) => {
              const file = event.target.files?.[0];
              if (file) void importSelected(file);
            }}
            ref={inputRef}
            type="file"
          />
        </div>
      </div>
      {assets.length === 0 ? (
        <div className="asset-empty">还没有图像资源。导入按钮图像后，可在脚本中使用 find_image。</div>
      ) : (
        <div className="asset-grid">
          {assets.map((asset) => (
            <article className="asset-card" key={asset.id}>
              <div className="asset-thumbnail">
                {previews[asset.id] ? (
                  <img alt={asset.name} src={previews[asset.id]} />
                ) : (
                  <span>{missing.has(asset.id) ? "文件不存在" : "读取中…"}</span>
                )}
              </div>
              <div className="asset-card-body">
                <strong title={asset.name}>{asset.name}</strong>
                <small>{asset.width} × {asset.height}px</small>
                <code title={asset.id}>{asset.id}</code>
                {missing.has(asset.id) ? <em>资源文件缺失</em> : null}
                <div className="asset-card-actions">
                  <button onClick={() => void copyId(asset)} type="button">复制 ID</button>
                  <button disabled={busy} onClick={() => void rename(asset)} type="button">重命名</button>
                  <button className="danger-link" disabled={busy} onClick={() => void remove(asset)} type="button">删除</button>
                </div>
              </div>
            </article>
          ))}
        </div>
      )}
      {notice ? <small className="asset-manager-notice">{notice}</small> : null}
    </section>
  );
}
