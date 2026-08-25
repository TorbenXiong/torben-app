import type { UserSettings } from "./types";

export function resolveLanguagePreference(language: UserSettings["language"]): "en" | "zh-CN" {
  if (language !== "system") {
    return language;
  }
  return navigator.language.toLowerCase().startsWith("zh") ? "zh-CN" : "en";
}

export function applyThemePreference(theme: UserSettings["theme"]): () => void {
  const media =
    theme === "system" && typeof window.matchMedia === "function"
      ? window.matchMedia("(prefers-color-scheme: dark)")
      : undefined;
  const apply = () => {
    document.documentElement.dataset.theme =
      theme === "system" ? (media?.matches === false ? "light" : "dark") : theme;
  };
  apply();
  media?.addEventListener("change", apply);
  return () => media?.removeEventListener("change", apply);
}
