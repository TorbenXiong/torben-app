import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
const listenMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn() }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn() }));

import {
  clearSelection,
  getOperationEvents,
  getVersions,
  installApp,
  onVersionCatalogUpdated,
  selectVersion,
  uninstallApp,
} from "../api";

describe("Tauri application lifecycle command mapping", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    window.__TAURI_INTERNALS__ = {};
  });

  afterEach(() => {
    delete window.__TAURI_INTERNALS__;
  });

  it("uses the stable Node lifecycle command names and camel-case payloads", async () => {
    const installation = {
      appId: "node",
      version: "24.19.0",
      sourceId: "node.official",
      scope: "managed",
      installPath: "C:/Torben/apps/node/24.19.0",
      installedAt: "2026-08-25T00:00:00Z",
      health: "healthy",
    };
    invokeMock
      .mockResolvedValueOnce([
        {
          version: "24.19.0",
          ltsName: "Krypton",
          releasedAt: "2026-08-03",
          recommended: true,
        },
      ])
      .mockResolvedValueOnce(installation)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([]);

    await expect(getVersions("node")).resolves.toHaveLength(1);
    await expect(installApp("node", "lts")).resolves.toEqual(installation);
    await selectVersion("node", "24.19.0");
    await clearSelection("node");
    await uninstallApp("node", "24.19.0");
    await expect(getOperationEvents()).resolves.toEqual([]);

    expect(invokeMock.mock.calls).toEqual([
      ["list_versions", { appId: "node" }],
      ["install_app", { appId: "node", version: "lts" }],
      ["select_version", { appId: "node", version: "24.19.0" }],
      ["clear_selection", { appId: "node" }],
      ["uninstall_app", { appId: "node", version: "24.19.0" }],
      ["list_operations"],
    ]);
  });

  it("subscribes to background version catalog updates", async () => {
    const callback = vi.fn();
    const unlisten = vi.fn();
    listenMock.mockImplementationOnce(async (_eventName, handler) => {
      handler({ payload: "temurin" });
      return unlisten;
    });

    const stopListening = await onVersionCatalogUpdated(callback);

    expect(listenMock).toHaveBeenCalledWith("version-catalog-updated", expect.any(Function));
    expect(callback).toHaveBeenCalledWith("temurin");
    stopListening();
    expect(unlisten).toHaveBeenCalledOnce();
  });
});
