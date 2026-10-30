const BUILTIN_INPUT_SETTINGS_SECTIONS: Readonly<Record<string, string>> = {
  "gesture.intent": "gestures",
};

export function hasBuiltinClientExtensionInput(inputId: string): boolean {
  return Object.hasOwn(BUILTIN_INPUT_SETTINGS_SECTIONS, inputId);
}

export function builtinClientExtensionInputSection(inputId: string): string | null {
  return BUILTIN_INPUT_SETTINGS_SECTIONS[inputId] ?? null;
}
