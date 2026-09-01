import { useEffect, useRef } from "react";

type AutoSaveValidator<T> = (draft: T) => string | undefined;

export function useAutoSave<T>(
  draft: T | null,
  saved: T | undefined,
  validate: AutoSaveValidator<T>,
  save: (draft: T) => Promise<void>,
  delay = 450,
) {
  const saveRef = useRef(save);
  const validateRef = useRef(validate);
  saveRef.current = save;
  validateRef.current = validate;

  const draftSnapshot = draft ? JSON.stringify(draft) : "";
  const savedSnapshot = saved ? JSON.stringify(saved) : "";

  useEffect(() => {
    if (!draft || draftSnapshot === savedSnapshot) return;
    if (validateRef.current(draft)) return;

    const nextDraft = draft;
    const timer = window.setTimeout(() => {
      void saveRef.current(nextDraft);
    }, delay);

    return () => window.clearTimeout(timer);
  }, [delay, draft, draftSnapshot, savedSnapshot]);
}
