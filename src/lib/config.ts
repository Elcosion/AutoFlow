import { useCallback, useEffect, useState } from "react";
import { defaultConfig, type AppConfig } from "../types/config";
import { getConfig, saveConfig } from "./tauri";

export function useAppConfig() {
  const [config, setConfig] = useState<AppConfig>(defaultConfig);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    void getConfig()
      .then((nextConfig) => {
        if (active) setConfig(nextConfig);
      })
      .catch((reason: unknown) => {
        if (active) setError(toErrorMessage(reason));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, []);

  const persist = useCallback(async (nextConfig: AppConfig) => {
    setSaving(true);
    setError(null);
    try {
      const saved = await saveConfig(nextConfig);
      setConfig(saved);
      return saved;
    } catch (reason) {
      setError(toErrorMessage(reason));
      throw reason;
    } finally {
      setSaving(false);
    }
  }, []);

  return { config, loading, saving, error, setError, persist };
}

export function toErrorMessage(reason: unknown): string {
  if (typeof reason === "string") return reason;
  if (reason && typeof reason === "object" && "message" in reason) {
    const message = (reason as { message?: unknown }).message;
    if (typeof message === "string") return message;
  }
  return "操作失败，请检查配置后重试";
}
