export function shouldHydrateSourceDraft(
  previousMacroId: string | null,
  nextMacroId: string | null,
  sourceDirty: boolean,
) {
  return previousMacroId !== nextMacroId || !sourceDirty;
}
