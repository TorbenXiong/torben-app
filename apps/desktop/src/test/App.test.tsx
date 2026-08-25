import { open } from "@tauri-apps/plugin-dialog";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { HashRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "../App";
import * as api from "../api";
import {
  checkTorbenUpdate,
  formatTorbenError,
  getSnapshot,
  initialTorbenUpdateStatus,
} from "../api";
import { commandShortcut } from "../components/Layout";
import i18n from "../i18n";
import {
  CatalogPage,
  CodexDetailPage,
  DiagnosticsPage,
  GitDetailPage,
  InstalledPage,
  OverviewPage,
  PluginsPage,
  PythonDetailPage,
  SettingsPage,
  TasksPage,
  TemurinDetailPage,
  VsCodeDetailPage,
} from "../pages";
import type {
  ApplicationDescriptor,
  InstallRecord,
  ManagedLibraryMigrationResult,
  ManagedToPackageMigrationPlan,
  ManagedToPackageMigrationResult,
  ManagedUpdateCandidate,
  OperationEvent,
  PackageInstallationRecord,
  PackageToManagedMigrationPlan,
  PackageToManagedMigrationResult,
  PluginSummary,
  SchemaPage,
  SelectionRecord,
  ShellIntegrationStatus,
  SourceAdapterStatus,
  SourceExecutionResult,
  SourceMigrationPlan,
  SourceMigrationResult,
  SourceOperationPlan,
  UpdatePreferences,
  UserSettings,
} from "../types";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

const bundledPlugin: PluginSummary = {
  id: "app.torben.plugin.node",
  displayName: "Node.js",
  version: "0.1.0",
  enabled: true,
  origin: "built_in",
  publisher: "Torben App",
  capabilities: ["version_discovery", "managed_install"],
  permissions: {
    networkDomains: ["nodejs.org"],
    filesystemRoots: ["managed_app_library"],
    externalCommands: ["node", "npm", "npx"],
    packageManagers: [],
  },
};

const sideloadedPlugin: PluginSummary = {
  id: "dev.example.fixture",
  displayName: "Fixture",
  version: "1.2.3",
  enabled: true,
  origin: "sideloaded",
  publisher: "Example Publisher",
  capabilities: ["schema_ui"],
  permissions: {
    networkDomains: ["example.invalid"],
    filesystemRoots: ["managed_app_library"],
    externalCommands: ["fixture"],
    packageManagers: ["npm"],
  },
};

const disabledShellIntegration: ShellIntegrationStatus = {
  state: "disabled",
  shimPath: "C:/Torben/tools/shims",
  targets: ["HKCU/Environment/Path"],
  newTerminalRequired: false,
};

const defaultUpdatePreferences: UpdatePreferences = {
  notifyTorbenApp: true,
  notifyManagedApps: true,
  automaticallyInstallTorbenApp: false,
  automaticallyUpdateApps: [],
};

afterEach(async () => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  await i18n.changeLanguage("en");
});

describe("Torben App shell", () => {
  it("uses native command shortcut labels for desktop platforms", () => {
    expect(commandShortcut("MacIntel")).toEqual({ aria: "Meta+K", label: "⌘ K" });
    expect(commandShortcut("Win32")).toEqual({ aria: "Control+K", label: "Ctrl K" });
    expect(commandShortcut("Linux x86_64")).toEqual({ aria: "Control+K", label: "Ctrl K" });
  });

  it("loads the local-first overview without a Tauri runtime", async () => {
    window.location.hash = "#/overview";
    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("Good evening, Torben.")).toBeInTheDocument();
    });
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(screen.getByText("Node.js, end to end")).toBeInTheDocument();
    expect(screen.getByText("Local-first")).toBeInTheDocument();

    const catalogLink = screen.getByRole("link", { name: /^catalog$/i });
    expect(catalogLink).toHaveClass("nav-item");
    expect(catalogLink.className).not.toContain("isActive");

    fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
    expect(screen.getByRole("button", { name: "Expand sidebar" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /^catalog$/i })).toBeInTheDocument();

    const skipLink = screen.getByRole("button", { name: "Skip to main content" });
    skipLink.focus();
    expect(skipLink).toHaveFocus();
    fireEvent.click(skipLink);
    expect(screen.getByRole("main")).toHaveFocus();
  });

  it("retries a failed initial snapshot without restarting the desktop", async () => {
    const snapshot = await getSnapshot();
    vi.spyOn(api, "getSnapshot")
      .mockRejectedValueOnce(new Error("Initial snapshot fixture failed"))
      .mockResolvedValue(snapshot);
    window.location.hash = "#/overview";
    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent("Initial snapshot fixture failed");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Good evening, Torben.")).toBeInTheDocument();
    expect(screen.queryByText("Initial snapshot fixture failed")).not.toBeInTheDocument();
  });

  it("keeps the desktop usable when one external discovery plugin fails", async () => {
    const snapshot = await getSnapshot();
    vi.spyOn(api, "getSnapshot").mockResolvedValue({
      ...snapshot,
      warnings: [
        {
          appId: "node",
          code: "plugin_response_malformed",
          message: "The Node.js plugin returned malformed data.",
          details: { method: "external.discover" },
          remediation: "Inspect the Node.js plugin and retry discovery.",
        },
      ],
    });
    window.location.hash = "#/overview";
    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    const warning = await screen.findByRole("status");
    expect(warning).toHaveTextContent(
      "External installation discovery failed for 1 application. Other local data remains available.",
    );
    expect(warning).toHaveTextContent(
      "node: [plugin_response_malformed] The Node.js plugin returned malformed data. Inspect the Node.js plugin and retry discovery.",
    );
    expect(screen.getByText("Good evening, Torben.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /^catalog$/i })).toBeInTheDocument();
  });

  it("clears a recovered task polling error without affecting the main snapshot", async () => {
    vi.spyOn(api, "getOperationEvents")
      .mockRejectedValueOnce(new Error("Task polling fixture failed"))
      .mockResolvedValue([]);
    vi.useFakeTimers();
    window.location.hash = "#/overview";
    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.getByText("Good evening, Torben.")).toBeInTheDocument();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(screen.getByRole("alert")).toHaveTextContent("Task polling fixture failed");
    expect(screen.getByText("Good evening, Torben.")).toBeInTheDocument();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(screen.queryByText("Task polling fixture failed")).not.toBeInTheDocument();
    expect(screen.getByText("Good evening, Torben.")).toBeInTheDocument();
  });

  it("does not overlap slow task polling requests", async () => {
    let completeFirstPoll: (events: OperationEvent[]) => void = () => undefined;
    const firstPoll = new Promise<OperationEvent[]>((resolve) => {
      completeFirstPoll = resolve;
    });
    const polling = vi
      .spyOn(api, "getOperationEvents")
      .mockReturnValueOnce(firstPoll)
      .mockResolvedValue([]);
    vi.useFakeTimers();
    window.location.hash = "#/overview";
    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    await act(async () => {
      await Promise.resolve();
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(polling).toHaveBeenCalledOnce();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(polling).toHaveBeenCalledOnce();

    await act(async () => {
      completeFirstPoll([]);
      await firstPoll;
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(polling).toHaveBeenCalledTimes(2);
  });

  it("searches pages and applications from the keyboard command palette", async () => {
    window.location.hash = "#/overview";
    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    await screen.findByRole("heading", { name: "Good evening, Torben." });
    const trigger = screen.getByRole("button", { name: "Search apps and commands" });
    expect(trigger).toHaveAttribute("aria-keyshortcuts", "Control+K");

    fireEvent.keyDown(window, { ctrlKey: true, key: "k" });
    const search = await screen.findByRole("combobox", { name: "Search apps and commands" });
    expect(search).toHaveFocus();

    const initialOptions = screen.getAllByRole("option");
    expect(initialOptions).toHaveLength(13);
    expect(initialOptions[0]).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(initialOptions[1]).toHaveAttribute("aria-selected", "true");

    fireEvent.change(search, { target: { value: "Visual Studio" } });
    const result = screen.getByRole("option", { name: /Visual Studio Code/ });
    expect(screen.getAllByRole("option")).toHaveLength(1);
    expect(result).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(search, { key: "Enter" });
    await waitFor(() => expect(window.location.hash).toBe("#/catalog/vscode"));
    expect(
      screen.queryByRole("dialog", { name: "Search apps and commands" }),
    ).not.toBeInTheDocument();
  });

  it("renders the overview navigation and main content in Simplified Chinese", async () => {
    await i18n.changeLanguage("zh-CN");
    const snapshot = await getSnapshot();

    const view = render(
      <HashRouter>
        <OverviewPage snapshot={snapshot} />
      </HashRouter>,
    );

    expect(screen.getByText("晚上好，Torben。")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /浏览应用目录/ })).toBeInTheDocument();
    expect(screen.getByText("受管安装")).toBeInTheDocument();
    expect(screen.getByText("1 项检查需关注")).toHaveClass("warning");
    expect(screen.getByText("近期操作")).toBeInTheDocument();

    view.rerender(
      <HashRouter>
        <OverviewPage
          snapshot={{
            ...snapshot,
            doctor: snapshot.doctor.map((check) => ({ ...check, healthy: true })),
          }}
        />
      </HashRouter>,
    );
    expect(screen.getByText("本地核心已就绪")).toHaveClass("positive");
  });

  it("shows only the latest event for each of the four most recent operations", async () => {
    const snapshot = await getSnapshot();
    const operation = (
      operationId: string,
      sequence: number,
      timestamp: string,
      phase: string,
    ): OperationEvent => ({
      operationId,
      sequence,
      state: "succeeded",
      phase,
      message: `${phase} message`,
      progress: 1,
      timestamp,
    });

    render(
      <HashRouter>
        <OverviewPage
          snapshot={{
            ...snapshot,
            operations: [
              operation("operation-a", 0, "6", "stale-a"),
              operation("operation-e", 0, "2", "oldest-e"),
              operation("operation-c", 0, "4", "latest-c"),
              operation("operation-a", 1, "7", "latest-a"),
              operation("operation-b", 0, "5", "latest-b"),
              operation("operation-d", 0, "3", "latest-d"),
            ],
          }}
        />
      </HashRouter>,
    );

    expect(screen.getByText("latest-a")).toBeInTheDocument();
    expect(screen.getByText("latest-b")).toBeInTheDocument();
    expect(screen.getByText("latest-c")).toBeInTheDocument();
    expect(screen.getByText("latest-d")).toBeInTheDocument();
    expect(screen.queryByText("stale-a")).not.toBeInTheDocument();
    expect(screen.queryByText("oldest-e")).not.toBeInTheDocument();
  });

  it("renders managed installation actions in Simplified Chinese", async () => {
    await i18n.changeLanguage("zh-CN");
    const records: InstallRecord[] = [
      {
        appId: "node",
        version: "24.19.0",
        sourceId: "node.official",
        scope: "managed",
        installPath: "C:/Torben/node/24.19.0",
        installedAt: "fixture",
        health: "healthy",
      },
    ];

    render(
      <InstalledPage
        external={[]}
        onChanged={async () => undefined}
        records={records}
        selected={[{ appId: "node", version: "24.19.0" }]}
      />,
    );

    expect(screen.getByRole("heading", { name: "已安装的应用" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "检查更新" })).toBeEnabled();
    expect(screen.getByText("已选择")).toBeInTheDocument();
    expect(screen.getByText("健康")).toBeInTheDocument();
    expect(screen.queryByText("healthy")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "清除" })).toBeEnabled();
  });

  it("preserves an unknown installation health diagnostic for investigation", () => {
    const record: InstallRecord = {
      appId: "node",
      version: "24.19.0",
      sourceId: "node.official",
      scope: "managed",
      installPath: "C:/Torben/node/24.19.0",
      installedAt: "fixture",
      health: "version output mismatch",
    };

    render(
      <InstalledPage
        external={[]}
        onChanged={async () => undefined}
        records={[record]}
        selected={[]}
      />,
    );

    expect(screen.getByText("version output mismatch")).toBeInTheDocument();
  });

  it("localizes built-in catalog metadata and searches by Chinese category", async () => {
    await i18n.changeLanguage("zh-CN");
    const snapshot = await getSnapshot();

    render(
      <HashRouter>
        <CatalogPage applications={snapshot.applications} />
      </HashRouter>,
    );

    expect(
      screen.getByText("提供受管 LTS 和 Current 版本的 JavaScript 运行时。"),
    ).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: "搜索应用目录" }), {
      target: { value: "编辑器" },
    });
    expect(screen.getByRole("heading", { name: "Visual Studio Code" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Node.js" })).not.toBeInTheDocument();
  });

  it("shows the selected managed version and requires clearing it before uninstall", () => {
    const records: InstallRecord[] = [
      {
        appId: "node",
        version: "24.19.0",
        sourceId: "node.official",
        scope: "managed",
        installPath: "C:/Torben/node/24.19.0",
        installedAt: "fixture",
        health: "healthy",
      },
    ];
    const selected: SelectionRecord[] = [{ appId: "node", version: "24.19.0" }];

    render(
      <InstalledPage
        external={[]}
        onChanged={async () => undefined}
        records={records}
        selected={selected}
      />,
    );

    expect(screen.getByText("Selected")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Clear" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Uninstall node 24.19.0" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Use" })).not.toBeInTheDocument();
  });

  it("uses the matching application icon for non-Node external installations", () => {
    const external: InstallRecord = {
      appId: "python",
      version: "3.14.7",
      sourceId: "python.external",
      scope: "external",
      installPath: "C:/Python314/python.exe",
      installedAt: "fixture",
      health: "healthy",
    };

    render(
      <InstalledPage
        external={[external]}
        onChanged={async () => undefined}
        records={[]}
        selected={[]}
      />,
    );

    expect(screen.getByText("Py")).toHaveClass("app-icon-python");
    expect(screen.queryByText("JS")).not.toBeInTheDocument();
    expect(screen.getByText("Read only")).toBeInTheDocument();
  });

  it("routes package-manager installations to source management", () => {
    const records: InstallRecord[] = [
      {
        appId: "vscode",
        version: "1.134.0",
        sourceId: "source.winget",
        scope: "package_manager",
        installPath: "C:/Program Files/Microsoft VS Code/Code.exe",
        installedAt: "fixture",
        health: "healthy",
      },
    ];

    render(
      <HashRouter>
        <InstalledPage
          external={[]}
          onChanged={async () => undefined}
          records={records}
          selected={[]}
        />
      </HashRouter>,
    );

    expect(screen.getByText("Package manager")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Manage source" })).toHaveAttribute(
      "href",
      "#/diagnostics",
    );
    expect(screen.queryByRole("button", { name: "Use" })).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Uninstall vscode 1.134.0" }),
    ).not.toBeInTheDocument();
  });

  it("shows release-line updates and requires an explicit per-app auto-update preference", async () => {
    const record: InstallRecord = {
      appId: "node",
      version: "24.19.0",
      sourceId: "node.official",
      scope: "managed",
      installPath: "C:/Torben/node/24.19.0",
      installedAt: "fixture",
      health: "healthy",
    };
    const candidate: ManagedUpdateCandidate = {
      appId: "node",
      channel: "24",
      installedVersion: "24.19.0",
      availableVersion: "24.20.1",
      selectedVersion: "24.19.0",
      releasedAt: "2026-08-24T00:00:00Z",
      recommended: true,
      automatic: false,
    };
    const onApplyUpdate = vi.fn(async () => ({
      candidate,
      installation: { ...record, version: "24.20.1" },
      selectionUpdated: true,
    }));
    const onAutoUpdateChange = vi.fn(async () => ({
      theme: "system" as const,
      language: "en" as const,
      updates: {
        ...defaultUpdatePreferences,
        automaticallyUpdateApps: ["node"],
      },
    }));
    const onCheckUpdates = vi.fn(async () => ({
      checkedApps: 1,
      candidates: [candidate],
      warnings: [],
    }));
    const onChanged = vi.fn(async () => undefined);

    render(
      <InstalledPage
        external={[]}
        onApplyUpdate={onApplyUpdate}
        onAutoUpdateChange={onAutoUpdateChange}
        onChanged={onChanged}
        onCheckUpdates={onCheckUpdates}
        records={[record]}
        selected={[{ appId: "node", version: "24.19.0" }]}
        settings={{ theme: "system", language: "en", updates: defaultUpdatePreferences }}
        updates={{ checkedApps: 1, candidates: [candidate], warnings: [] }}
      />,
    );

    expect(screen.getByText("24.19.0 → 24.20.1 · channel 24")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Enable automatic updates for node" }));
    await waitFor(() => {
      expect(onAutoUpdateChange).toHaveBeenCalledWith("node", true);
      expect(onCheckUpdates).toHaveBeenCalledOnce();
    });

    fireEvent.click(screen.getByRole("button", { name: "Update node to 24.20.1" }));
    await waitFor(() => {
      expect(onApplyUpdate).toHaveBeenCalledWith(candidate);
      expect(onChanged).toHaveBeenCalledOnce();
      expect(onCheckUpdates).toHaveBeenCalledTimes(2);
    });
  });

  it("shows Eclipse Temurin LTS releases and Java command integration", async () => {
    render(<TemurinDetailPage installed={[]} onChanged={async () => undefined} />);

    await waitFor(() => {
      expect(screen.getByText("Eclipse Temurin")).toBeInTheDocument();
      expect(screen.getByText("v21.0.2+13.0.LTS")).toBeInTheDocument();
    });
    expect(screen.getByText(/java and javac resolve/)).toBeInTheDocument();
    expect(screen.getByText("OpenPGP + SHA-256")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Refresh Eclipse Temurin versions" })).toBeEnabled();
  });

  it("shows stable CPython releases and pip command integration", async () => {
    render(<PythonDetailPage installed={[]} onChanged={async () => undefined} />);

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "Python" })).toBeInTheDocument();
      expect(screen.getByText("v3.14.7")).toBeInTheDocument();
    });
    expect(screen.getAllByText("Stable").length).toBeGreaterThan(0);
    expect(screen.getByText(/python, python3, pip, and pip3 resolve/)).toBeInTheDocument();
    expect(screen.getByText("Signed catalog / Sigstore")).toBeInTheDocument();
  });

  it("shows the managed Git CLI release and terminal command integration", async () => {
    render(<GitDetailPage installed={[]} onChanged={async () => undefined} />);

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "Git" })).toBeInTheDocument();
      expect(screen.getByText("v2.55.0+windows.5")).toBeInTheDocument();
    });
    expect(screen.getByText(/git resolves through one managed shim directory/)).toBeInTheDocument();
    expect(screen.getByText("Signed metadata / SHA-256")).toBeInTheDocument();
  });

  it("shows stable Visual Studio Code releases with managed updates disabled", async () => {
    render(<VsCodeDetailPage installed={[]} onChanged={async () => undefined} />);

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "Visual Studio Code" })).toBeInTheDocument();
      expect(screen.getByText("v1.134.0")).toBeInTheDocument();
    });
    expect(
      screen.getByText(/code resolves through one managed shim directory/),
    ).toBeInTheDocument();
    expect(screen.getByText("Microsoft metadata / SHA-256")).toBeInTheDocument();
  });

  it("shows native Codex releases without claiming authentication state", async () => {
    render(<CodexDetailPage installed={[]} onChanged={async () => undefined} />);

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "Codex CLI" })).toBeInTheDocument();
      expect(screen.getByText("v0.149.1")).toBeInTheDocument();
    });
    expect(screen.getByText(/CODEX_HOME stays user-owned/)).toBeInTheDocument();
    expect(screen.getByText("GitHub SHA-256 / Linux Sigstore")).toBeInTheDocument();
  });

  it("preserves structured Core error codes and remediation in the UI", () => {
    expect(
      formatTorbenError({
        code: "version_is_selected",
        message: "The selected version cannot be uninstalled.",
        remediation: "Clear the selection first.",
      }),
    ).toBe(
      "[version_is_selected] The selected version cannot be uninstalled. Clear the selection first.",
    );
  });

  it("keeps development builds offline when no updater key was compiled", async () => {
    const configuration = {
      configured: false,
      currentVersion: "0.1.0",
      endpoint: "https://github.com/TorbenXiong/torben-app/releases/latest/download/latest.json",
    };
    expect(initialTorbenUpdateStatus(configuration).state).toBe("unconfigured");
    await expect(checkTorbenUpdate(configuration)).resolves.toMatchObject({
      state: "unconfigured",
      currentVersion: "0.1.0",
      availableVersion: null,
    });
  });

  it("shows package-manager availability and confirmed execution diagnostics", async () => {
    const sourceAdapters: SourceAdapterStatus[] = [
      {
        adapter: "winget",
        sourceId: "source.winget",
        availability: "available",
        executable: "winget.exe",
        version: "Windows Package Manager v1.12",
        supportsExactVersion: true,
        requiresElevation: false,
        message: "Package manager is available.",
      },
      {
        adapter: "apt",
        sourceId: "source.apt",
        availability: "unsupported",
        executable: null,
        version: null,
        supportsExactVersion: true,
        requiresElevation: true,
        message: "This adapter is not supported on the current operating system.",
      },
    ];
    const refresh = vi.fn(async () => undefined);

    render(<DiagnosticsPage checks={[]} onChanged={refresh} sourceAdapters={sourceAdapters} />);

    expect(screen.getByText("Plan + confirm")).toBeInTheDocument();
    expect(screen.getByText("Windows Package Manager v1.12")).toBeInTheDocument();
    expect(screen.getByText(/External authorization required/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
    await waitFor(() => expect(refresh).toHaveBeenCalledOnce());
  });

  it("executes only an accepted and reconfirmed package-manager plan", async () => {
    const applications: ApplicationDescriptor[] = [
      {
        id: "vscode",
        displayName: "Visual Studio Code",
        summary: "fixture",
        categories: ["Editor"],
        capabilities: [],
        sources: [],
      },
    ];
    const sourceAdapters: SourceAdapterStatus[] = [
      {
        adapter: "winget",
        sourceId: "source.winget",
        availability: "available",
        executable: "winget.exe",
        version: "Windows Package Manager v1.12",
        supportsExactVersion: true,
        requiresElevation: false,
        message: "Package manager is available.",
      },
    ];
    const plan: SourceOperationPlan = {
      action: "install",
      adapter: "winget",
      sourceId: "source.winget",
      coordinate: "Microsoft.VisualStudioCode",
      packageKind: "native",
      packageVersion: "1.134.0",
      executable: "winget.exe",
      previewArguments: ["show", "--id", "Microsoft.VisualStudioCode"],
      executeArguments: ["install", "--id", "Microsoft.VisualStudioCode", "--version", "1.134.0"],
      executionIdentity: null,
      environment: {},
      requiresElevation: false,
      exactVersionGuaranteed: true,
      mutatesSystem: true,
      warnings: ["Shared package-manager state may change."],
    };
    const result: SourceExecutionResult = {
      operationId: "00000000-0000-0000-0000-000000000001",
      plan,
      before: {
        adapter: "winget",
        sourceId: "source.winget",
        coordinate: "Microsoft.VisualStudioCode",
        packageKind: "native",
        installed: false,
        installedVersion: null,
        architecture: null,
        managerOwned: false,
      },
      after: {
        adapter: "winget",
        sourceId: "source.winget",
        coordinate: "Microsoft.VisualStudioCode",
        packageKind: "native",
        installed: true,
        installedVersion: "1.134.0",
        architecture: "x64",
        managerOwned: true,
      },
      outcome: "ownership_committed",
      installation: null,
    };
    const planSource = vi.fn(async () => plan);
    const executeSource = vi.fn(async () => result);
    const onChanged = vi.fn(async () => undefined);

    render(
      <DiagnosticsPage
        applications={applications}
        checks={[]}
        executeSource={executeSource}
        onChanged={onChanged}
        planSource={planSource}
        sourceAdapters={sourceAdapters}
      />,
    );

    fireEvent.change(screen.getByLabelText("Application version"), {
      target: { value: "1.134.0" },
    });
    fireEvent.change(screen.getByLabelText("Package coordinate"), {
      target: { value: "Microsoft.VisualStudioCode" },
    });
    fireEvent.change(screen.getByLabelText("Raw package version"), {
      target: { value: "1.134.0" },
    });
    fireEvent.change(screen.getByLabelText("Installed executable path"), {
      target: { value: "C:\\fixture\\code.exe" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Review plan" }));

    await screen.findByText(/winget\.exe install --id Microsoft\.VisualStudioCode/);
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this exact plan and accept its system changes.",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Execute change" }));
    const executeButtons = screen.getAllByRole("button", { name: "Execute change" });
    fireEvent.click(executeButtons[executeButtons.length - 1]);

    await waitFor(() => expect(executeSource).toHaveBeenCalledOnce());
    expect(executeSource).toHaveBeenCalledWith({
      appId: "vscode",
      appVersion: "1.134.0",
      action: "install",
      adapter: "winget",
      coordinate: "Microsoft.VisualStudioCode",
      packageKind: "native",
      packageVersion: "1.134.0",
      executablePath: "C:\\fixture\\code.exe",
      approvedExecutionIdentity: null,
      acceptSystemChanges: true,
    });
    expect(onChanged).toHaveBeenCalledOnce();
  });

  it("reviews cleanup and restore commands before migrating immutable source ownership", async () => {
    const operation = (
      action: "install" | "uninstall",
      adapter: "apt" | "dnf",
      coordinate: string,
      version: string,
    ): SourceOperationPlan => ({
      action,
      adapter,
      sourceId: `source.${adapter}`,
      coordinate,
      packageKind: "native",
      packageVersion: version,
      executable: adapter,
      previewArguments: ["info", coordinate],
      executeArguments: [action === "install" ? "install" : "remove", coordinate],
      executionIdentity: adapter === "dnf" ? `code-${version}.x86_64` : null,
      environment: {},
      requiresElevation: true,
      exactVersionGuaranteed: true,
      mutatesSystem: true,
      warnings: [],
    });
    const currentOwner = {
      appId: "vscode",
      appVersion: "1.134.0",
      sourceId: "source.apt",
      adapter: "apt" as const,
      coordinate: "code-old",
      packageKind: "native" as const,
      packageVersion: "1.134.0",
      architecture: "amd64",
      executablePath: "/usr/local/bin/code",
      ownedByTorben: true,
      installedAt: "fixture",
      health: "healthy",
    };
    const plan: SourceMigrationPlan = {
      appId: "vscode",
      appVersion: "1.134.0",
      currentOwner,
      currentState: {
        adapter: "apt",
        sourceId: "source.apt",
        coordinate: "code-old",
        packageKind: "native",
        installed: true,
        installedVersion: "1.134.0",
        architecture: "amd64",
        managerOwned: true,
      },
      targetState: {
        adapter: "dnf",
        sourceId: "source.dnf",
        coordinate: "code",
        packageKind: "native",
        installed: false,
        installedVersion: null,
        architecture: null,
        managerOwned: false,
      },
      uninstallCurrent: operation("uninstall", "apt", "code-old", "1.134.0"),
      installTarget: operation("install", "dnf", "code", "1.134.0-1.fc42"),
      cleanupTarget: operation("uninstall", "dnf", "code", "1.134.0-1.fc42"),
      restoreCurrent: operation("install", "apt", "code-old", "1.134.0"),
      targetExecutablePath: "/usr/bin/code",
      approvalToken: "fixture-migration-token",
      warnings: ["Application configuration is not migrated."],
    };
    const result: SourceMigrationResult = {
      operationId: "00000000-0000-0000-0000-000000000002",
      plan,
      installation: { ...currentOwner, sourceId: "source.dnf", adapter: "dnf", coordinate: "code" },
    };
    const planMigration = vi.fn(async () => plan);
    const executeMigration = vi.fn(async () => result);
    const onChanged = vi.fn(async () => undefined);

    render(
      <DiagnosticsPage
        applications={[
          {
            id: "vscode",
            displayName: "Visual Studio Code",
            summary: "fixture",
            categories: [],
            capabilities: [],
            sources: [],
          },
        ]}
        checks={[]}
        executeMigration={executeMigration}
        onChanged={onChanged}
        packageInstallations={[currentOwner]}
        planMigration={planMigration}
        sourceAdapters={[
          {
            adapter: "dnf",
            sourceId: "source.dnf",
            availability: "available",
            executable: "dnf",
            version: "dnf 5",
            supportsExactVersion: true,
            requiresElevation: true,
            message: "available",
          },
        ]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Prepare migration" }));
    fireEvent.change(screen.getByLabelText("Package coordinate"), { target: { value: "code" } });
    fireEvent.change(screen.getByLabelText("Raw package version"), {
      target: { value: "1.134.0-1.fc42" },
    });
    fireEvent.change(screen.getByLabelText("Installed executable path"), {
      target: { value: "/usr/bin/code" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Review plan" }));

    await screen.findByText("fixture-migration-token");
    expect(screen.getByText("Failure cleanup")).toBeInTheDocument();
    expect(screen.getByText("Failure restore")).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this exact plan and accept its system changes.",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Execute change" }));
    const confirmations = screen.getAllByRole("button", { name: "Execute change" });
    fireEvent.click(confirmations[confirmations.length - 1]);

    await waitFor(() => expect(executeMigration).toHaveBeenCalledOnce());
    expect(executeMigration).toHaveBeenCalledWith({
      appId: "vscode",
      appVersion: "1.134.0",
      targetAdapter: "dnf",
      targetCoordinate: "code",
      targetPackageKind: "native",
      targetPackageVersion: "1.134.0-1.fc42",
      targetExecutablePath: "/usr/bin/code",
      approvedPlanToken: "fixture-migration-token",
      acceptSystemChanges: true,
    });
    expect(onChanged).toHaveBeenCalledOnce();
  });

  it("stages a managed installation before migrating it to a reviewed package source", async () => {
    const managed: InstallRecord = {
      appId: "vscode",
      version: "1.134.0",
      sourceId: "vscode.official",
      scope: "managed",
      installPath: "D:\\Torben\\apps\\vscode\\1.134.0",
      installedAt: "fixture",
      health: "healthy",
    };
    const installTarget: SourceOperationPlan = {
      action: "install",
      adapter: "dnf",
      sourceId: "source.dnf",
      coordinate: "code",
      packageKind: "native",
      packageVersion: "1.134.0-1.fc42",
      executable: "dnf",
      previewArguments: ["info", "code-1.134.0-1.fc42.x86_64"],
      executeArguments: ["install", "code-1.134.0-1.fc42.x86_64"],
      executionIdentity: "code-1.134.0-1.fc42.x86_64",
      environment: {},
      requiresElevation: true,
      exactVersionGuaranteed: true,
      mutatesSystem: true,
      warnings: [],
    };
    const plan: ManagedToPackageMigrationPlan = {
      appId: "vscode",
      appVersion: "1.134.0",
      currentInstallation: managed,
      uninstallCurrent: {
        appId: "vscode",
        version: "1.134.0",
        sourceId: "vscode.official",
        installPath: managed.installPath,
        preserveUserData: true,
      },
      targetState: {
        adapter: "dnf",
        sourceId: "source.dnf",
        coordinate: "code",
        packageKind: "native",
        installed: false,
        installedVersion: null,
        architecture: null,
        managerOwned: false,
      },
      installTarget,
      cleanupTarget: { ...installTarget, action: "uninstall" },
      targetExecutablePath: "/usr/bin/code",
      approvalToken: "managed-package-token",
      warnings: ["The managed directory is staged for rollback."],
    };
    const installation = {
      appId: "vscode",
      appVersion: "1.134.0",
      sourceId: "source.dnf",
      adapter: "dnf" as const,
      coordinate: "code",
      packageKind: "native" as const,
      packageVersion: "1.134.0-1.fc42",
      architecture: "x86_64",
      executablePath: "/usr/bin/code",
      ownedByTorben: true,
      installedAt: "fixture",
      health: "healthy",
    };
    const result: ManagedToPackageMigrationResult = {
      operationId: "00000000-0000-0000-0000-000000000003",
      plan,
      installation,
    };
    const planManagedMigration = vi.fn(async () => plan);
    const executeManagedMigration = vi.fn(async () => result);

    render(
      <DiagnosticsPage
        applications={[
          {
            id: "vscode",
            displayName: "Visual Studio Code",
            summary: "fixture",
            categories: [],
            capabilities: [],
            sources: [],
          },
        ]}
        checks={[]}
        executeManagedMigration={executeManagedMigration}
        installed={[managed]}
        onChanged={async () => undefined}
        planManagedMigration={planManagedMigration}
        sourceAdapters={[
          {
            adapter: "dnf",
            sourceId: "source.dnf",
            availability: "available",
            executable: "dnf",
            version: "dnf 5",
            supportsExactVersion: true,
            requiresElevation: true,
            message: "available",
          },
        ]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Move to package source" }));
    fireEvent.change(screen.getByLabelText("Package coordinate"), { target: { value: "code" } });
    fireEvent.change(screen.getByLabelText("Raw package version"), {
      target: { value: "1.134.0-1.fc42" },
    });
    fireEvent.change(screen.getByLabelText("Installed executable path"), {
      target: { value: "/usr/bin/code" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Review plan" }));

    await screen.findByText("managed-package-token");
    expect(screen.getByText(managed.installPath)).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this exact plan and accept its system changes.",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Execute change" }));
    const confirmations = screen.getAllByRole("button", { name: "Execute change" });
    fireEvent.click(confirmations[confirmations.length - 1]);

    await waitFor(() => expect(executeManagedMigration).toHaveBeenCalledOnce());
    expect(executeManagedMigration).toHaveBeenCalledWith({
      appId: "vscode",
      appVersion: "1.134.0",
      targetAdapter: "dnf",
      targetCoordinate: "code",
      targetPackageKind: "native",
      targetPackageVersion: "1.134.0-1.fc42",
      targetExecutablePath: "/usr/bin/code",
      approvedPlanToken: "managed-package-token",
      acceptSystemChanges: true,
    });
  });

  it("installs the official archive before migrating a package source to managed ownership", async () => {
    const owner: PackageInstallationRecord = {
      appId: "vscode",
      appVersion: "1.134.0",
      sourceId: "source.dnf",
      adapter: "dnf",
      coordinate: "code",
      packageKind: "native",
      packageVersion: "1.134.0-1.fc42",
      architecture: "x86_64",
      executablePath: "/usr/bin/code",
      ownedByTorben: true,
      installedAt: "fixture",
      health: "healthy",
    };
    const command: SourceOperationPlan = {
      action: "uninstall",
      adapter: "dnf",
      sourceId: "source.dnf",
      coordinate: "code",
      packageKind: "native",
      packageVersion: owner.packageVersion,
      executable: "dnf",
      previewArguments: ["remove", "code-1.134.0-1.fc42.x86_64"],
      executeArguments: ["remove", "code-1.134.0-1.fc42.x86_64"],
      executionIdentity: "code-1.134.0-1.fc42.x86_64",
      environment: {},
      requiresElevation: true,
      exactVersionGuaranteed: true,
      mutatesSystem: true,
      warnings: [],
    };
    const managed: InstallRecord = {
      appId: "vscode",
      version: "1.134.0",
      sourceId: "vscode.official",
      scope: "managed",
      installPath: "D:\\Torben\\apps\\vscode\\1.134.0",
      installedAt: "fixture",
      health: "healthy",
    };
    const plan: PackageToManagedMigrationPlan = {
      appId: owner.appId,
      appVersion: owner.appVersion,
      currentOwner: owner,
      currentState: {
        adapter: owner.adapter,
        sourceId: owner.sourceId,
        coordinate: owner.coordinate,
        packageKind: owner.packageKind,
        installed: true,
        installedVersion: owner.packageVersion,
        architecture: owner.architecture,
        managerOwned: true,
      },
      uninstallCurrent: command,
      restoreCurrent: { ...command, action: "install" },
      installManaged: {
        appId: owner.appId,
        version: owner.appVersion,
        sourceId: managed.sourceId,
        steps: [],
        metadata: {},
      },
      managedTargetPath: managed.installPath,
      approvalToken: "package-managed-token",
      warnings: ["The managed archive is verified before package removal."],
    };
    const result: PackageToManagedMigrationResult = {
      operationId: "00000000-0000-0000-0000-000000000004",
      plan,
      installation: managed,
    };
    const planPackageMigration = vi.fn(async () => plan);
    const executePackageMigration = vi.fn(async () => result);

    render(
      <DiagnosticsPage
        applications={[
          {
            id: "vscode",
            displayName: "Visual Studio Code",
            summary: "fixture",
            categories: [],
            capabilities: [],
            sources: [],
          },
        ]}
        checks={[]}
        executePackageMigration={executePackageMigration}
        onChanged={async () => undefined}
        packageInstallations={[owner]}
        planPackageMigration={planPackageMigration}
        sourceAdapters={[
          {
            adapter: "dnf",
            sourceId: "source.dnf",
            availability: "available",
            executable: "dnf",
            version: "dnf 5",
            supportsExactVersion: true,
            requiresElevation: true,
            message: "available",
          },
        ]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Move to managed source" }));
    fireEvent.click(screen.getByRole("button", { name: "Review plan" }));

    await screen.findByText("package-managed-token");
    expect(screen.getByText(managed.installPath)).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this exact plan and accept its system changes.",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Execute change" }));
    const confirmations = screen.getAllByRole("button", { name: "Execute change" });
    fireEvent.click(confirmations[confirmations.length - 1]);

    await waitFor(() => expect(executePackageMigration).toHaveBeenCalledOnce());
    expect(executePackageMigration).toHaveBeenCalledWith({
      appId: "vscode",
      appVersion: "1.134.0",
      approvedPlanToken: "package-managed-token",
      acceptSystemChanges: true,
    });
  });

  it("enables DNF only after displaying the locked NEVRA", async () => {
    const plan: SourceOperationPlan = {
      action: "install",
      adapter: "dnf",
      sourceId: "source.dnf",
      coordinate: "code",
      packageKind: "native",
      packageVersion: "1.134.0-1.fc42",
      executable: "dnf",
      previewArguments: ["info", "code"],
      executeArguments: ["install", "code-1.134.0-1.fc42.x86_64"],
      executionIdentity: "code-1.134.0-1.fc42.x86_64",
      environment: {},
      requiresElevation: true,
      exactVersionGuaranteed: true,
      mutatesSystem: true,
      warnings: [],
    };

    render(
      <DiagnosticsPage
        applications={[
          {
            id: "vscode",
            displayName: "Visual Studio Code",
            summary: "fixture",
            categories: [],
            capabilities: [],
            sources: [],
          },
        ]}
        checks={[]}
        onChanged={async () => undefined}
        planSource={vi.fn(async () => plan)}
        sourceAdapters={[
          {
            adapter: "dnf",
            sourceId: "source.dnf",
            availability: "available",
            executable: "dnf",
            version: "dnf 5",
            supportsExactVersion: true,
            requiresElevation: true,
            message: "Package manager is available.",
          },
        ]}
      />,
    );

    fireEvent.change(screen.getByLabelText("Application version"), {
      target: { value: "1.134.0" },
    });
    fireEvent.change(screen.getByLabelText("Package coordinate"), {
      target: { value: "code" },
    });
    fireEvent.change(screen.getByLabelText("Raw package version"), {
      target: { value: "1.134.0-1.fc42" },
    });
    fireEvent.change(screen.getByLabelText("Installed executable path"), {
      target: { value: "/usr/bin/code" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Review plan" }));

    await screen.findByText("code-1.134.0-1.fc42.x86_64");
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this exact plan and accept its system changes.",
      }),
    );
    expect(screen.getByRole("button", { name: "Execute change" })).toBeEnabled();
  });

  it("shows bundled plugin permissions and keeps its state immutable", () => {
    render(<PluginsPage onChanged={async () => undefined} plugins={[bundledPlugin]} />);

    expect(screen.getByText("nodejs.org")).toBeInTheDocument();
    expect(screen.getByText("managed_app_library")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Node.js is a bundled plugin" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Install plugin" }));
    expect(screen.getByText("Install a developer-mode plugin?")).toBeInTheDocument();
    expect(
      screen.getByText("Developer mode bypasses the signed official plugin registry."),
    ).toBeInTheDocument();
  });

  it("toggles sideloaded plugins through the shared Core action", async () => {
    const changeEnabled = vi.fn(async () => undefined);
    const onChanged = vi.fn(async () => undefined);
    render(
      <PluginsPage
        changeEnabled={changeEnabled}
        onChanged={onChanged}
        plugins={[sideloadedPlugin]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Disable Fixture" }));

    await waitFor(() => {
      expect(changeEnabled).toHaveBeenCalledWith("dev.example.fixture", false);
      expect(onChanged).toHaveBeenCalledOnce();
    });
  });

  it("distinguishes an official registry plugin from a sideloaded plugin", () => {
    const officialPlugin: PluginSummary = {
      ...sideloadedPlugin,
      id: "app.example.official",
      displayName: "Official fixture",
      origin: "official_registry",
    };

    render(
      <PluginsPage
        onChanged={async () => undefined}
        plugins={[officialPlugin, sideloadedPlugin]}
      />,
    );

    expect(screen.getByText("Official registry")).toBeInTheDocument();
    expect(screen.getByText("Sideloaded")).toBeInTheDocument();
  });

  it("refreshes and installs through the configured official registry", async () => {
    const refreshRegistry = vi.fn(async () => ({
      configured: true,
      sourceUrl: "https://plugins.example/registry.json",
      cachePath: "C:/Torben/cache/registry.json",
      sequence: 8,
      generatedAt: "2026-08-23T00:00:00Z",
    }));
    const installRegistryPlugin = vi.fn(async () => sideloadedPlugin);
    const onChanged = vi.fn(async () => undefined);
    render(
      <PluginsPage
        installRegistryPlugin={installRegistryPlugin}
        onChanged={onChanged}
        plugins={[bundledPlugin]}
        refreshRegistry={refreshRegistry}
        registry={{
          configured: true,
          sourceUrl: "https://plugins.example/registry.json",
          cachePath: "C:/Torben/cache/registry.json",
          sequence: 7,
          generatedAt: "2026-08-22T00:00:00Z",
        }}
      />,
    );

    expect(screen.getByText(/Trusted sequence 7/)).toHaveTextContent(
      new Date("2026-08-22T00:00:00Z").toLocaleString("en"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(refreshRegistry).toHaveBeenCalledOnce());

    fireEvent.click(screen.getByRole("button", { name: "Install official" }));
    fireEvent.change(screen.getByLabelText("Plugin ID"), {
      target: { value: "app.example.official" },
    });
    fireEvent.change(screen.getByLabelText("Exact version (optional)"), {
      target: { value: "1.2.3" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Install" }));

    await waitFor(() => {
      expect(installRegistryPlugin).toHaveBeenCalledWith("app.example.official", "1.2.3");
      expect(onChanged).toHaveBeenCalledTimes(2);
    });
  });

  it("renders schema fields and confirms destructive plugin actions", async () => {
    const page: SchemaPage = {
      id: "settings",
      title: "Plugin settings",
      description: "Schema-driven fixture settings",
      sections: [
        {
          id: "general",
          title: "General",
          description: null,
          fields: [
            {
              id: "channel",
              label: "Channel",
              description: null,
              kind: "select",
              value: "lts",
              placeholder: null,
              options: [
                { value: "lts", label: "LTS" },
                { value: "current", label: "Current" },
              ],
              readOnly: false,
              required: true,
            },
            {
              id: "enabled",
              label: "Enabled",
              description: null,
              kind: "boolean",
              value: "false",
              placeholder: null,
              options: [],
              readOnly: false,
              required: true,
            },
            {
              id: "status",
              label: "Status",
              description: null,
              kind: "status",
              value: "Ready",
              placeholder: null,
              options: [],
              readOnly: true,
              required: false,
            },
          ],
          actions: [
            {
              id: "reset",
              label: "Reset",
              description: null,
              kind: "destructive",
              enabled: true,
              confirmation: "Reset this plugin?",
            },
          ],
        },
      ],
    };
    const loadSchemaPages = vi.fn(async () => [page]);
    const runSchemaAction = vi.fn(async () => ({
      pluginId: sideloadedPlugin.id,
      page,
      message: "Reset complete",
    }));
    const onChanged = vi.fn(async () => undefined);
    render(
      <PluginsPage
        loadSchemaPages={loadSchemaPages}
        onChanged={onChanged}
        plugins={[sideloadedPlugin]}
        runSchemaAction={runSchemaAction}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Open Fixture pages" }));
    await waitFor(() => expect(screen.getByText("Plugin settings")).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText(/^Channel/), { target: { value: "current" } });
    fireEvent.click(screen.getByLabelText(/^Enabled/));
    fireEvent.click(screen.getByRole("button", { name: "Reset" }));

    expect(screen.getByText("Reset this plugin?")).toBeInTheDocument();
    expect(runSchemaAction).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    await waitFor(() => {
      expect(runSchemaAction).toHaveBeenCalledWith(
        sideloadedPlugin.id,
        "settings",
        "general",
        "reset",
        { channel: "current", enabled: "true" },
        true,
      );
      expect(screen.getByText("Reset complete")).toBeInTheDocument();
      expect(onChanged).toHaveBeenCalledOnce();
    });
  });

  it("does not install when manifest selection is cancelled", async () => {
    const chooseManifest = vi.fn(async () => null);
    const installManifest = vi.fn(async () => sideloadedPlugin);
    render(
      <PluginsPage
        chooseManifest={chooseManifest}
        installManifest={installManifest}
        onChanged={async () => undefined}
        plugins={[bundledPlugin]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Install plugin" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose manifest" }));

    await waitFor(() => expect(chooseManifest).toHaveBeenCalledOnce());
    expect(installManifest).not.toHaveBeenCalled();
  });

  it("uses explicit developer mode after the trust confirmation", async () => {
    const installManifest = vi.fn(async () => sideloadedPlugin);
    const onChanged = vi.fn(async () => undefined);
    render(
      <PluginsPage
        chooseManifest={async () => "C:/fixture/plugin.json"}
        installManifest={installManifest}
        onChanged={onChanged}
        plugins={[bundledPlugin]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Install plugin" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose manifest" }));

    await waitFor(() => {
      expect(installManifest).toHaveBeenCalledWith("C:/fixture/plugin.json", true);
      expect(onChanged).toHaveBeenCalledOnce();
    });
  });

  it("shows the latest task event and requests cancellation through Core", async () => {
    const events: OperationEvent[] = [
      {
        operationId: "11111111-1111-4111-8111-111111111111",
        sequence: 2,
        state: "running",
        phase: "download",
        message: "Downloading archive",
        progress: 0.3,
        timestamp: "2",
      },
      {
        operationId: "11111111-1111-4111-8111-111111111111",
        sequence: 0,
        state: "running",
        phase: "prepare",
        message: "Operation started",
        progress: 0,
        timestamp: "1",
      },
    ];
    const cancel = vi.fn(async () => undefined);
    const onChanged = vi.fn(async () => undefined);
    render(<TasksPage cancel={cancel} events={events} onChanged={onChanged} />);

    expect(screen.getByText("download")).toBeInTheDocument();
    expect(screen.queryByText("prepare")).not.toBeInTheDocument();
    expect(screen.getByText("Running")).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Progress for download" })).toHaveAttribute(
      "aria-valuenow",
      "30",
    );
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    await waitFor(() => {
      expect(cancel).toHaveBeenCalledWith("11111111-1111-4111-8111-111111111111");
      expect(onChanged).toHaveBeenCalledOnce();
    });
    expect(screen.getByRole("button", { name: "Cancelling…" })).toBeDisabled();
  });

  it("formats task timestamps with the selected application language", async () => {
    await i18n.changeLanguage("zh-CN");
    const timestamp = "1700000000";
    render(
      <TasksPage
        events={[
          {
            operationId: "22222222-2222-4222-8222-222222222222",
            sequence: 1,
            state: "succeeded",
            phase: "commit",
            message: "Installation committed",
            progress: 1,
            timestamp,
          },
        ]}
        onChanged={async () => undefined}
      />,
    );

    expect(
      screen.getByText(new Date(Number(timestamp) * 1000).toLocaleString("zh-CN")),
    ).toBeInTheDocument();
    expect(screen.getByText("已成功")).toBeInTheDocument();
  });

  it("persists appearance settings through the shared desktop action", async () => {
    await i18n.changeLanguage("en");
    const settings: UserSettings = {
      theme: "system",
      language: "system",
      updates: defaultUpdatePreferences,
    };
    const onChange = vi.fn(async () => undefined);
    render(
      <SettingsPage
        onChange={onChange}
        settings={settings}
        shellIntegration={disabledShellIntegration}
      />,
    );

    fireEvent.change(screen.getByRole("combobox", { name: "Theme" }), {
      target: { value: "light" },
    });

    await waitFor(() => {
      expect(onChange).toHaveBeenCalledWith({
        theme: "light",
        language: "system",
        updates: defaultUpdatePreferences,
      });
    });
  });

  it("shows one local alert when saving settings fails", async () => {
    vi.spyOn(api, "updateSettings").mockRejectedValue(new Error("Settings save fixture failed"));
    window.location.hash = "#/settings";
    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    await screen.findByRole("heading", { name: "Settings" });
    fireEvent.change(screen.getByRole("combobox", { name: "Theme" }), {
      target: { value: "light" },
    });

    await waitFor(() => {
      const alerts = screen.getAllByRole("alert");
      expect(alerts).toHaveLength(1);
      expect(alerts[0]).toHaveTextContent("Settings save fixture failed");
    });
  });

  it("localizes development placeholder paths without changing real paths", async () => {
    await i18n.changeLanguage("zh-CN");
    render(
      <SettingsPage
        settings={{ theme: "system", language: "zh-CN", updates: defaultUpdatePreferences }}
        shellIntegration={{
          ...disabledShellIntegration,
          shimPath: "Platform data directory/tools/shims",
        }}
      />,
    );

    expect(screen.getByText("平台数据目录/tools/shims")).toBeInTheDocument();
    expect(screen.getByText("平台数据目录/apps")).toBeInTheDocument();
    expect(screen.queryByText("Platform data directory/apps")).not.toBeInTheDocument();
  });

  it("reports a committed library migration whose old source still needs cleanup", async () => {
    await i18n.changeLanguage("en");
    vi.mocked(open).mockResolvedValue("D:\\Torben Apps");
    const result: ManagedLibraryMigrationResult = {
      previousPath: "C:\\Old Torben Apps",
      currentPath: "D:\\Torben Apps",
      bytesCopied: 42,
      sourceCleanupPending: true,
    };
    const onLibraryMigrate = vi.fn(async () => result);
    render(
      <SettingsPage
        onLibraryMigrate={onLibraryMigrate}
        settings={{ theme: "system", language: "en", updates: defaultUpdatePreferences }}
        shellIntegration={disabledShellIntegration}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Migrate library" }));

    await waitFor(() => {
      expect(onLibraryMigrate).toHaveBeenCalledWith("D:\\Torben Apps");
      expect(
        screen.getByText(
          "Application library migrated to D:\\Torben Apps. The old library could not be removed and will be retried the next time Torben App starts.",
        ),
      ).toHaveClass("warning-text");
    });
  });

  it("persists notify-only update preferences without enabling background installation", async () => {
    await i18n.changeLanguage("en");
    const onChange = vi.fn(async () => undefined);
    render(
      <SettingsPage
        onChange={onChange}
        settings={{ theme: "system", language: "en", updates: defaultUpdatePreferences }}
        shellIntegration={disabledShellIntegration}
      />,
    );

    fireEvent.change(screen.getByRole("combobox", { name: "Torben App" }), {
      target: { value: "disabled" },
    });

    await waitFor(() => {
      expect(onChange).toHaveBeenCalledWith({
        theme: "system",
        language: "en",
        updates: { ...defaultUpdatePreferences, notifyTorbenApp: false },
      });
    });
    expect(screen.getByText("Background service")).toBeInTheDocument();
  });

  it("requires an explicit action before installing a signed Torben App update", async () => {
    await i18n.changeLanguage("en");
    const onUpdateCheck = vi.fn(async () => undefined);
    const onInstallTorbenUpdate = vi.fn(async () => undefined);
    render(
      <SettingsPage
        onInstallTorbenUpdate={onInstallTorbenUpdate}
        onUpdateCheck={onUpdateCheck}
        settings={{ theme: "system", language: "en", updates: defaultUpdatePreferences }}
        shellIntegration={disabledShellIntegration}
        updater={{
          configured: true,
          currentVersion: "0.1.0",
          endpoint:
            "https://github.com/TorbenXiong/torben-app/releases/latest/download/latest.json",
        }}
        updateStatus={{
          state: "available",
          currentVersion: "0.1.0",
          availableVersion: "0.2.0",
          publishedAt: "2026-08-24T00:00:00Z",
          notes: "Signed update fixture",
          progress: null,
          message: "Torben App 0.2.0 is available.",
        }}
      />,
    );

    expect(onInstallTorbenUpdate).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Check now" }));
    fireEvent.click(screen.getByRole("button", { name: "Download and install" }));
    await waitFor(() => {
      expect(onUpdateCheck).toHaveBeenCalledOnce();
      expect(onInstallTorbenUpdate).toHaveBeenCalledOnce();
    });
  });

  it("requires an explicit settings action to enable user PATH integration", async () => {
    await i18n.changeLanguage("en");
    const onShellChange = vi.fn(async () => undefined);
    render(
      <SettingsPage
        onShellChange={onShellChange}
        settings={{ theme: "system", language: "en", updates: defaultUpdatePreferences }}
        shellIntegration={disabledShellIntegration}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Enable user PATH integration" }));

    await waitFor(() => expect(onShellChange).toHaveBeenCalledWith(true));
  });

  it("does not remove a shim path configured outside Torben App", async () => {
    await i18n.changeLanguage("en");
    const onShellChange = vi.fn(async () => undefined);
    render(
      <SettingsPage
        onShellChange={onShellChange}
        settings={{ theme: "system", language: "en", updates: defaultUpdatePreferences }}
        shellIntegration={{ ...disabledShellIntegration, state: "external" }}
      />,
    );

    expect(screen.getByRole("button", { name: "Enable user PATH integration" })).toBeDisabled();
    expect(
      screen.getByText(
        "This path was configured outside Torben App and will never be removed automatically.",
      ),
    ).toBeInTheDocument();
    expect(onShellChange).not.toHaveBeenCalled();
  });
});
