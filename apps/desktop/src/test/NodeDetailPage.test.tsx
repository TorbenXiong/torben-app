import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as api from "../api";
import i18n from "../i18n";
import { NodeDetailPage } from "../NodeDetailPage";
import type { InstallRecord, OperationEvent } from "../types";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("shows exact Node releases as independent installations within one major line", async () => {
  await i18n.changeLanguage("en");
  vi.spyOn(api, "getVersions").mockResolvedValue([
    { version: "24.20.1", releasedAt: "2026-09-01", ltsName: "Krypton", recommended: true },
  ]);
  vi.spyOn(api, "onVersionCatalogUpdated").mockResolvedValue(() => undefined);
  const installed: InstallRecord = {
    appId: "node",
    version: "24.19.0",
    sourceId: "node.official",
    scope: "managed",
    installPath: "D:/TorbenApp/userData/apps/node/24.19.0",
    installedAt: "fixture",
    health: "healthy",
  };
  const install = vi.spyOn(api, "installApp").mockResolvedValue(installed);

  render(
    <NodeDetailPage installed={[installed]} selected={[]} onChanged={async () => undefined} />,
  );

  expect(await screen.findAllByText("Node.js 24")).toHaveLength(2);
  expect(screen.getByText("v24.19.0")).toBeInTheDocument();
  const availableRow = screen.getByText("v24.20.1").closest(".version-row");
  expect(availableRow).not.toBeNull();
  fireEvent.click(within(availableRow as HTMLElement).getByRole("button", { name: "Install" }));
  await waitFor(() => expect(install).toHaveBeenCalledWith("node", "24.20.1"));
});

it("keeps other Node versions clickable and shows progress on each installing row", async () => {
  await i18n.changeLanguage("en");
  vi.spyOn(api, "getVersions").mockResolvedValue([
    { version: "24.20.1", releasedAt: "2026-09-01", ltsName: "Krypton", recommended: true },
    { version: "22.22.3", releasedAt: "2026-08-01", ltsName: "Jod", recommended: true },
  ]);
  vi.spyOn(api, "onVersionCatalogUpdated").mockResolvedValue(() => undefined);
  const pending = new Map<string, () => void>();
  const install = vi.spyOn(api, "installApp").mockImplementation(
    (_appId, version) =>
      new Promise<InstallRecord>((resolve) => {
        pending.set(version, () =>
          resolve({
            appId: "node",
            version,
            sourceId: "node.official",
            scope: "managed",
            installPath: `D:/TorbenApp/userData/apps/node/${version}`,
            installedAt: "fixture",
            health: "healthy",
          }),
        );
      }),
  );
  const progress: OperationEvent = {
    operationId: "018f-test-operation",
    sequence: 3,
    state: "running",
    phase: "download",
    message: "Downloading node-v24.20.1-win-x64.zip",
    progress: 0.42,
    timestamp: "2026-09-10T00:00:00Z",
    kind: "install",
    appId: "node",
    version: "24.20.1",
  };

  const { rerender } = render(
    <NodeDetailPage installed={[]} selected={[]} onChanged={async () => undefined} />,
  );
  const firstRow = (await screen.findByText("v24.20.1")).closest(".version-row") as HTMLElement;
  const secondRow = screen.getByText("v22.22.3").closest(".version-row") as HTMLElement;
  fireEvent.click(within(firstRow).getByRole("button", { name: "Install" }));

  expect(await within(firstRow).findByRole("button", { name: "Installing…" })).toBeDisabled();
  expect(within(secondRow).getByRole("button", { name: "Install" })).toBeEnabled();
  fireEvent.click(within(secondRow).getByRole("button", { name: "Install" }));
  await waitFor(() => {
    expect(install).toHaveBeenCalledWith("node", "24.20.1");
    expect(install).toHaveBeenCalledWith("node", "22.22.3");
  });

  rerender(
    <NodeDetailPage
      installed={[]}
      operations={[progress]}
      selected={[]}
      onChanged={async () => undefined}
    />,
  );
  const progressBar = within(firstRow).getByRole("progressbar", {
    name: "Installation progress for 24.20.1",
  });
  expect(progressBar).toHaveAttribute("aria-valuenow", "42");
  expect(within(firstRow).getByText("Downloading node-v24.20.1-win-x64.zip")).toBeInTheDocument();

  pending.get("24.20.1")?.();
  pending.get("22.22.3")?.();
});

it("installs exact Node releases, selects and clears the terminal version, and confirms uninstall", async () => {
  await i18n.changeLanguage("en");
  vi.spyOn(api, "getVersions").mockResolvedValue([
    { version: "24.19.0", releasedAt: "2026-08-25", ltsName: "Krypton", recommended: true },
  ]);
  vi.spyOn(api, "onVersionCatalogUpdated").mockResolvedValue(() => undefined);
  const record: InstallRecord = {
    appId: "node",
    version: "24.19.0",
    sourceId: "node.official",
    scope: "managed",
    installPath: "D:/TorbenApp/userData/apps/node/24.19.0",
    installedAt: "fixture",
    health: "healthy",
  };
  const install = vi.spyOn(api, "installApp").mockResolvedValue(record);
  const select = vi.spyOn(api, "selectVersion").mockResolvedValue(undefined);
  const clear = vi.spyOn(api, "clearSelection").mockResolvedValue(undefined);
  const uninstall = vi.spyOn(api, "uninstallApp").mockResolvedValue(undefined);
  const onChanged = vi.fn(async () => undefined);
  const { rerender } = render(
    <NodeDetailPage installed={[]} selected={[]} onChanged={onChanged} />,
  );
  expect(await screen.findByText("v24.19.0")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Install" }));
  await waitFor(() => expect(install).toHaveBeenCalledWith("node", "24.19.0"));
  await waitFor(() => expect(screen.getByRole("button", { name: "Install" })).toBeEnabled());
  rerender(<NodeDetailPage installed={[record]} selected={[]} onChanged={onChanged} />);
  fireEvent.click(screen.getByRole("button", { name: "Set as primary version" }));
  await waitFor(() => expect(select).toHaveBeenCalledWith("node", "24.19.0"));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Set as primary version" })).toBeEnabled(),
  );
  rerender(
    <NodeDetailPage
      installed={[record]}
      selected={[{ appId: "node", version: "24.19.0" }]}
      onChanged={onChanged}
    />,
  );
  expect(screen.getByRole("button", { name: "Uninstall" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: "Clear primary version" }));
  await waitFor(() => expect(clear).toHaveBeenCalledWith("node"));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Clear primary version" })).toBeEnabled(),
  );
  rerender(<NodeDetailPage installed={[record]} selected={[]} onChanged={onChanged} />);
  fireEvent.click(screen.getByRole("button", { name: "Uninstall" }));
  expect(uninstall).not.toHaveBeenCalled();
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Uninstall" }));
  await waitFor(() => expect(uninstall).toHaveBeenCalledWith("node", "24.19.0"));
});

it("shows Core errors and keeps installation retryable", async () => {
  vi.spyOn(api, "getVersions").mockResolvedValue([
    { version: "24.19.0", releasedAt: "2026-08-25", ltsName: "Krypton", recommended: true },
  ]);
  vi.spyOn(api, "onVersionCatalogUpdated").mockResolvedValue(() => undefined);
  vi.spyOn(api, "installApp").mockRejectedValue({
    code: "archive_hash_mismatch",
    message: "Checksum mismatch",
  });
  render(<NodeDetailPage installed={[]} selected={[]} onChanged={async () => undefined} />);
  fireEvent.click(await screen.findByRole("button", { name: "Install" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("archive_hash_mismatch");
  await waitFor(() => expect(screen.getByRole("button", { name: "Install" })).toBeEnabled());
});
