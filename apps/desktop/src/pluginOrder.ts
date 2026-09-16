import type { PluginSummary } from "./types";

export function comparePluginOrder(leftId: string, rightId: string, order: readonly string[]) {
  const leftIndex = order.indexOf(leftId);
  const rightIndex = order.indexOf(rightId);
  return (
    (leftIndex < 0 ? Number.MAX_SAFE_INTEGER : leftIndex) -
    (rightIndex < 0 ? Number.MAX_SAFE_INTEGER : rightIndex)
  );
}

export function normalizePluginOrder(order: readonly string[], plugins: readonly PluginSummary[]) {
  const pluginIds = new Set(plugins.map((plugin) => plugin.id));
  const normalized = order.filter((pluginId) => pluginIds.has(pluginId));
  const remaining = plugins
    .filter((plugin) => !normalized.includes(plugin.id))
    .sort((left, right) => Number(right.enabled) - Number(left.enabled))
    .map((plugin) => plugin.id);
  return [...new Set([...normalized, ...remaining])];
}

export function movePlugin(order: readonly string[], pluginId: string, targetPluginId: string) {
  if (pluginId === targetPluginId) return [...order];
  const next = [...order];
  const fromIndex = next.indexOf(pluginId);
  const targetIndex = next.indexOf(targetPluginId);
  if (fromIndex < 0 || targetIndex < 0) return next;
  next.splice(fromIndex, 1);
  const insertionIndex = next.indexOf(targetPluginId) + (fromIndex < targetIndex ? 1 : 0);
  next.splice(insertionIndex, 0, pluginId);
  return next;
}
