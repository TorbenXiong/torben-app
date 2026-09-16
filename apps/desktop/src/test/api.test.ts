import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
const listenMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn() }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn() }));

import {
  backupDatabaseInstance,
  clearSelection,
  createDatabaseInstance,
  deleteDatabaseInstance,
  getOperationEvents,
  getVersions,
  installApp,
  installBundledNodePlugin,
  installBundledPythonPlugin,
  listDatabaseInstances,
  onVersionCatalogUpdated,
  refreshDatabaseInstanceStatus,
  restoreDatabaseInstance,
  selectVersion,
  startDatabaseInstance,
  stopDatabaseInstance,
  uninstallApp,
  uninstallBundledNodePlugin,
  uninstallBundledPythonPlugin,
} from "../api";

describe("Tauri application lifecycle command mapping", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    window.__TAURI_INTERNALS__ = {};
  });

  it("maps the complete managed database instance lifecycle", async () => {
    const instance = {
      engine: "redis",
      name: "local",
      runtimeVersion: "8.2.1",
      port: 6379,
      dataPath: "C:/Torben/application-data/redis/instances/local/data",
      createdAt: "fixture",
      state: "stopped",
      pid: null,
    } as const;
    const target = { engine: "redis", name: "local" } as const;
    invokeMock
      .mockResolvedValueOnce([instance])
      .mockResolvedValueOnce(instance)
      .mockResolvedValueOnce({ ...instance, state: "running", pid: 42 })
      .mockResolvedValueOnce({ ...instance, state: "running", pid: 42 })
      .mockResolvedValueOnce({
        engine: "redis",
        instanceName: "local",
        path: "C:/backup/local.rdb",
        createdAt: "fixture",
      })
      .mockResolvedValueOnce(instance)
      .mockResolvedValueOnce(instance)
      .mockResolvedValueOnce(undefined);

    await listDatabaseInstances("redis");
    await createDatabaseInstance({
      engine: "redis",
      name: "local",
      runtimeVersion: "8.2.1",
      port: 6379,
    });
    await startDatabaseInstance(target);
    await refreshDatabaseInstanceStatus(target);
    await backupDatabaseInstance(target);
    await restoreDatabaseInstance(target, "C:/backup/local.rdb");
    await stopDatabaseInstance(target);
    await deleteDatabaseInstance(target);

    expect(invokeMock.mock.calls).toEqual([
      ["list_database_instances", { engine: "redis" }],
      [
        "create_database_instance",
        {
          request: {
            engine: "redis",
            name: "local",
            runtimeVersion: "8.2.1",
            port: 6379,
          },
        },
      ],
      ["start_database_instance", { target }],
      ["database_instance_status", { target }],
      ["backup_database_instance", { request: { ...target, destination: null } }],
      ["restore_database_instance", { request: { ...target, source: "C:/backup/local.rdb" } }],
      ["stop_database_instance", { target }],
      ["delete_database_instance", { request: { ...target, confirm: true } }],
    ]);
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

  it("maps bundled Python plugin lifecycle commands", async () => {
    const plugin = { id: "app.torben.plugin.python" };
    invokeMock.mockResolvedValueOnce(plugin).mockResolvedValueOnce(undefined);

    await expect(installBundledPythonPlugin()).resolves.toEqual(plugin);
    await uninstallBundledPythonPlugin();

    expect(invokeMock.mock.calls).toEqual([
      ["install_bundled_python_plugin"],
      ["uninstall_bundled_python_plugin"],
    ]);
  });

  it("maps bundled Node plugin lifecycle commands", async () => {
    const plugin = { id: "app.torben.plugin.node" };
    invokeMock.mockResolvedValueOnce(plugin).mockResolvedValueOnce(undefined);

    await expect(installBundledNodePlugin()).resolves.toEqual(plugin);
    await uninstallBundledNodePlugin();

    expect(invokeMock.mock.calls).toEqual([
      ["install_bundled_node_plugin"],
      ["uninstall_bundled_node_plugin"],
    ]);
  });
});
