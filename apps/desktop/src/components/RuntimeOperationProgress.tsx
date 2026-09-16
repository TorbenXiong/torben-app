import { ProgressBar } from "@torben-app/ui";
import { useTranslation } from "react-i18next";
import type { OperationEvent } from "../types";

export function activeRuntimeOperation(
  events: OperationEvent[],
  appId: string,
  version: string,
): OperationEvent | undefined {
  const latest = new Map<string, OperationEvent>();
  for (const event of events) {
    const current = latest.get(event.operationId);
    if (!current || event.sequence > current.sequence) latest.set(event.operationId, event);
  }
  return [...latest.values()]
    .filter(
      (event) =>
        (event.kind === "install" || event.kind === "uninstall") &&
        event.appId === appId &&
        event.version === version &&
        (event.state === "pending" || event.state === "running" || event.state === "cancelling"),
    )
    .sort((left, right) => right.timestamp.localeCompare(left.timestamp))[0];
}

export function RuntimeOperationProgress({
  event,
  pending,
  pendingLabel,
  version,
}: {
  event?: OperationEvent;
  pending: boolean;
  pendingLabel?: string;
  version: string;
}) {
  const { t } = useTranslation();
  if (!pending && !event) return null;
  const progress = Math.round((event?.progress ?? 0.02) * 100);
  const phase = event?.message ?? pendingLabel ?? t("common.installing");
  return (
    <div className="runtime-operation-progress">
      <span>{phase}</span>
      <span>{progress}%</span>
      <ProgressBar label={t("runtimePage.installProgress", { version })} value={progress} />
    </div>
  );
}
