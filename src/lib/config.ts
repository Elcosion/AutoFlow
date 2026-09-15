import { useCallback, useEffect, useRef, useState } from "react";
import { defaultConfig, type AppConfig } from "../types/config";
import { getConfig, saveConfig } from "./tauri";

export function useAppConfig() {
  const [config, setConfig] = useState<AppConfig>(defaultConfig);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const savingRef = useRef(false);
  const configRequestGenerationRef = useRef(0);

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

  useEffect(() => {
    let timer: number | null = null;
    const reloadExternalFiles = () => {
      if (savingRef.current) return;
      if (timer !== null) window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        if (savingRef.current) return;
        const requestGeneration = configRequestGenerationRef.current;
        void getConfig()
          .then((nextConfig) => {
            if (
              !savingRef.current &&
              configRequestGenerationRef.current === requestGeneration
            ) {
              setConfig(nextConfig);
            }
          })
          .catch((reason: unknown) => setError(toErrorMessage(reason)));
      }, 300);
    };
    window.addEventListener("focus", reloadExternalFiles);
    return () => {
      window.removeEventListener("focus", reloadExternalFiles);
      if (timer !== null) window.clearTimeout(timer);
    };
  }, []);

  const persist = useCallback(async (nextConfig: AppConfig) => {
    const requestGeneration = configRequestGenerationRef.current + 1;
    configRequestGenerationRef.current = requestGeneration;
    savingRef.current = true;
    setSaving(true);
    setError(null);
    try {
      const saved = await saveConfig(nextConfig);
      if (configRequestGenerationRef.current === requestGeneration) {
        setConfig(saved);
      }
      return saved;
    } catch (reason) {
      setError(toErrorMessage(reason));
      throw reason;
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  }, []);

  const refresh = useCallback(async () => {
    const requestGeneration = configRequestGenerationRef.current + 1;
    configRequestGenerationRef.current = requestGeneration;
    const nextConfig = await getConfig();
    if (configRequestGenerationRef.current === requestGeneration) {
      setConfig(nextConfig);
    }
    return nextConfig;
  }, []);

  return { config, loading, saving, error, setError, persist, refresh };
}

export function toErrorMessage(reason: unknown): string {
  if (typeof reason === "string") return reason;
  if (reason && typeof reason === "object" && "message" in reason) {
    const message = (reason as { message?: unknown }).message;
    if (typeof message === "string") return message;
  }
  return "操作失败，请检查配置后重试";
}
