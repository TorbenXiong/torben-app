import { open } from "@tauri-apps/plugin-dialog";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
import { commandShortcut, Layout } from "../components/Layout";
import i18n from "../i18n";
import {
  DiagnosticsPage,
  LogsPage,
  MysqlDetailPage,
  PluginDetailPage,
  PluginsPage,
  PythonDetailPage,
  SettingsPage,
  TemurinDetailPage,
} from "../pages";
import type {
  ApplicationDescriptor,
  InstallRecord,
  ManagedLibraryMigrationResult,
  ManagedToPackageMigrationPlan,
  ManagedToPackageMigrationResult,
  OperationEvent,
  PackageInstallationRecord,
  PackageToManagedMigrationPlan,
  PackageToManagedMigrationResult,
  PluginSummary,
  SchemaPage,
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
  version: "0.0.1",
  enabled: true,
  origin: "built_in",
  publisher: "Torben App",
  capabilities: ["version_discovery", "managed_install"],
  permissions: {
    networkDomains: ["nodejs.org"],
    filesystemRoots: ["managed_app_library"],
    externalCommands: ["node", "npm", "npx", "pnpm"],
    packageManagers: [],
  },
};

const installedTemurinPlugin: PluginSummary = {
  id: "app.torben.plugin.temurin",
  displayName: "Java",
  version: "0.0.1",
  enabled: true,
  origin: "built_in",
  publisher: "Torben App",
  capabilities: ["version_discovery", "managed_install", "schema_ui"],
  permissions: {
    networkDomains: ["api.adoptium.net"],
    filesystemRoots: ["managed_app_library"],
    externalCommands: ["java", "javac"],
    packageManagers: [],
  },
};

const availableTemurinPlugin: PluginSummary = {
  ...installedTemurinPlugin,
  enabled: false,
};

const installedPythonPlugin: PluginSummary = {
  id: "app.torben.plugin.python",
  displayName: "Python",
  version: "0.0.1",
  enabled: true,
  origin: "built_in",
  publisher: "Torben App",
  capabilities: ["version_discovery", "managed_install", "global_selection", "schema_ui"],
  permissions: {
    networkDomains: ["www.python.org"],
    filesystemRoots: ["managed_app_library"],
    externalCommands: ["python", "python3", "pip", "pip3"],
    packageManagers: [],
  },
};

const availablePythonPlugin: PluginSummary = {
  ...installedPythonPlugin,
  enabled: false,
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

const defaultUserSettings: UserSettings = {
  theme: "system",
  language: "en",
  updates: defaultUpdatePreferences,
  pluginOrder: [],
  applicationEnvironments: {},
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

  it("opens plugins first and omits the removed navigation pages", async () => {
    window.location.hash = "#/overview";
    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "Plugins" })).toBeInTheDocument();
    });
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(screen.getByText("Local-first")).toBeInTheDocument();

    const primaryNavigation = screen.getByRole("navigation", { name: "Primary navigation" });
    expect(within(primaryNavigation).getAllByRole("link")[0]).toHaveAccessibleName("Plugins");
    const collapseButton = screen.getByRole("button", { name: "Collapse sidebar" });
    const backButton = screen.getByRole("button", { name: "Go back" });
    const forwardButton = screen.getByRole("button", { name: "Go forward" });
    const helpButton = screen.getByRole("button", { name: "Help" });
    const settingsLink = screen.getByRole("link", { name: "Settings" });
    expect(screen.getByRole("button", { name: "Minimize window" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Maximize or restore window" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close window" })).toBeInTheDocument();
    expect(collapseButton.compareDocumentPosition(backButton)).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );
    expect(backButton.compareDocumentPosition(forwardButton)).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );
    expect(forwardButton.compareDocumentPosition(helpButton)).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );
    expect(within(primaryNavigation).queryByRole("link", { name: "Logs" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Diagnostics" })).not.toBeInTheDocument();
    fireEvent.pointerDown(helpButton, { button: 0, ctrlKey: false });
    expect(await screen.findByRole("menuitem", { name: "Diagnostics" })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Logs" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "About Torben App" }));
    const aboutDialog = screen.getByRole("dialog", { name: "Torben App" });
    expect(aboutDialog).toHaveTextContent("Version 0.0.1");
    expect(aboutDialog).toHaveTextContent("local-first application manager for Windows");
    fireEvent.click(within(aboutDialog).getByText("Close", { selector: "button" }));
    expect(screen.queryByRole("dialog", { name: "Torben App" })).not.toBeInTheDocument();
    expect(screen.queryByText("Local core")).not.toBeInTheDocument();
    expect(settingsLink.closest(".sidebar-footer")).not.toBeNull();
    expect(screen.queryByRole("link", { name: /^overview$/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /^catalog$/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /^installed$/i })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
    expect(screen.getByRole("button", { name: "Expand sidebar" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /^plugins$/i })).toBeInTheDocument();

    const skipLink = screen.getByRole("button", { name: "Skip to main content" });
    skipLink.focus();
    expect(skipLink).toHaveFocus();
    fireEvent.click(skipLink);
    expect(screen.getByRole("main")).toHaveFocus();
  });

  it("adds Java to the sidebar when the Java plugin is installed", async () => {
    const snapshot = await getSnapshot();
    vi.spyOn(api, "getSnapshot").mockResolvedValue({
      ...snapshot,
      applications: snapshot.applications.map((application) =>
        application.id === "temurin" ? { ...application, capabilities: ["versions"] } : application,
      ),
      plugins: [installedTemurinPlugin],
    });
    window.location.hash = "#/overview";

    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    await screen.findByRole("heading", { name: "Plugins" });
    const javaLink = screen.getByRole("link", { name: "Java" });
    expect(screen.getByText("Installed")).toBeInTheDocument();
    expect(javaLink).toHaveClass("nav-item-child");
    expect(javaLink).toHaveAttribute("href", "#/java");
    expect(javaLink.querySelector(".java-nav-icon")).toHaveAttribute("src", "/icons/duke.png");

    fireEvent.click(javaLink);
    expect(await screen.findByRole("heading", { name: "Available versions" })).toBeInTheDocument();
  });

  it("adds Python to the sidebar when the Python plugin is installed", async () => {
    const snapshot = await getSnapshot();
    vi.spyOn(api, "getSnapshot").mockResolvedValue({
      ...snapshot,
      applications: snapshot.applications.map((application) =>
        application.id === "python" ? { ...application, capabilities: ["versions"] } : application,
      ),
      plugins: [installedPythonPlugin],
    });
    window.location.hash = "#/overview";

    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    await screen.findByRole("heading", { name: "Plugins" });
    expect(screen.getByRole("link", { name: /^Python$/ })).toHaveAttribute("href", "#/python");
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
    expect(await screen.findByRole("heading", { name: "Plugins" })).toBeInTheDocument();
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
    expect(await screen.findByRole("heading", { name: "Plugins" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /^plugins$/i })).toBeInTheDocument();
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
    expect(screen.getByRole("heading", { name: "Plugins" })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("Task polling fixture failed");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(screen.getByRole("heading", { name: "Plugins" })).toBeInTheDocument();
    expect(screen.queryByText("Task polling fixture failed")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Plugins" })).toBeInTheDocument();
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

  it("omits unsupported applications from the keyboard command palette", async () => {
    window.location.hash = "#/overview";
    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    await screen.findByRole("heading", { name: "Plugins" });
    const trigger = screen.getByRole("button", { name: "Search apps and commands" });
    expect(trigger).toHaveAttribute("aria-keyshortcuts", "Control+K");

    fireEvent.keyDown(window, { ctrlKey: true, key: "k" });
    const search = await screen.findByRole("combobox", { name: "Search apps and commands" });
    expect(search).toHaveFocus();

    const initialOptions = screen.getAllByRole("option");
    expect(initialOptions).toHaveLength(4);
    expect(initialOptions[0]).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(initialOptions[1]).toHaveAttribute("aria-selected", "true");

    fireEvent.change(search, { target: { value: "Visual Studio" } });
    expect(screen.queryByRole("option")).not.toBeInTheDocument();

    fireEvent.change(search, { target: { value: "Plugins" } });
    const result = screen.getByRole("option", { name: /Plugins/ });
    expect(screen.getAllByRole("option")).toHaveLength(1);
    expect(result).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(search, { key: "Enter" });
    await waitFor(() => expect(window.location.hash).toBe("#/plugins"));
    expect(
      screen.queryByRole("dialog", { name: "Search apps and commands" }),
    ).not.toBeInTheDocument();
  });

  it("redirects removed application routes to plugins", async () => {
    const versions = vi.spyOn(api, "getVersions");
    window.location.hash = "#/catalog/node";

    render(
      <HashRouter>
        <App />
      </HashRouter>,
    );

    expect(await screen.findByRole("heading", { name: "Plugins" })).toBeInTheDocument();
    await waitFor(() => expect(window.location.hash).toBe("#/plugins"));
    expect(versions).not.toHaveBeenCalled();
  });

  it("shows Java LTS releases without the redundant introduction cards", async () => {
    render(<TemurinDetailPage installed={[]} onChanged={async () => undefined} />);

    await waitFor(() => {
      expect(screen.getByText("v21.0.2+13.0.LTS")).toBeInTheDocument();
    });
    expect(screen.queryByText("Terminal commands")).not.toBeInTheDocument();
    expect(screen.queryByText("Transactional storage")).not.toBeInTheDocument();
    expect(screen.queryByText("Source ownership")).not.toBeInTheDocument();
    expect(screen.getByText("JDK 21")).toBeInTheDocument();
    expect(screen.queryByText("Recommended")).not.toBeInTheDocument();
  });

  it("switches database applications between version and instance management tabs", async () => {
    vi.spyOn(api, "getVersions").mockResolvedValue([
      {
        version: "5.7.44",
        releasedAt: "2023-10-25T00:00:00Z",
        recommended: false,
      },
    ]);
    const listInstances = vi.spyOn(api, "listDatabaseInstances").mockResolvedValue([]);

    render(<MysqlDetailPage installed={[]} onChanged={async () => undefined} />);

    expect(await screen.findByRole("tab", { name: "Version management" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByText("v5.7.44")).toBeInTheDocument();
    expect(screen.queryByText("Legacy")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: "Instance management" }));
    expect(await screen.findByRole("heading", { name: "Managed instances" })).toBeInTheDocument();
    expect(screen.queryByText("v5.7.44")).not.toBeInTheDocument();
    expect(listInstances).toHaveBeenCalledWith("mysql");

    fireEvent.click(screen.getByRole("tab", { name: "Version management" }));
    expect(screen.getByText("v5.7.44")).toBeInTheDocument();
  });

  it("keeps Java releases independently installable within the same JDK line", async () => {
    vi.spyOn(api, "getVersions").mockResolvedValue([
      {
        version: "25.0.3+9.0.LTS",
        ltsName: "Java 25 LTS",
        releasedAt: "2026-04-21T00:00:00Z",
        recommended: false,
      },
      {
        version: "25.0.4+101.0.LTS",
        ltsName: "Java 25 LTS",
        releasedAt: "2026-07-21T00:00:00Z",
        recommended: false,
      },
      {
        version: "21.0.12+101.0.LTS",
        ltsName: "Java 21 LTS",
        releasedAt: "2026-07-21T00:00:00Z",
        recommended: false,
      },
    ]);
    const record: InstallRecord = {
      appId: "temurin",
      version: "25.0.3+9.0.LTS",
      sourceId: "temurin.official",
      scope: "managed",
      installPath: "C:/Torben/temurin/25.0.3+9.0.LTS",
      installedAt: "fixture",
      health: "healthy",
    };
    const install = vi.spyOn(api, "installApp").mockResolvedValue({} as InstallRecord);
    const onChanged = vi.fn(async () => undefined);

    render(<TemurinDetailPage installed={[record]} onChanged={onChanged} selected={[record]} />);

    expect(await screen.findAllByText("JDK 25")).toHaveLength(2);
    expect(screen.getByText("v25.0.3+9.0.LTS")).toBeInTheDocument();
    expect(screen.getByText("v25.0.4+101.0.LTS")).toBeInTheDocument();
    const newestRow = screen.getByText("v25.0.4+101.0.LTS").closest(".version-row");
    expect(newestRow).not.toBeNull();
    fireEvent.click(within(newestRow as HTMLElement).getByRole("button", { name: "Install" }));

    await waitFor(() => {
      expect(install).toHaveBeenCalledWith("temurin", "25.0.4+101.0.LTS");
      expect(onChanged).toHaveBeenCalledOnce();
    });
  });

  it("sets an installed Java runtime as the primary terminal version", async () => {
    vi.spyOn(api, "getVersions").mockResolvedValue([
      {
        version: "21.0.2+13.0.LTS",
        ltsName: "Java 21 LTS",
        releasedAt: "2026-01-20T00:00:00Z",
        recommended: false,
      },
    ]);
    const select = vi.spyOn(api, "selectVersion").mockResolvedValue(undefined);
    const enableShell = vi.spyOn(api, "setShellIntegration").mockResolvedValue({
      state: "managed",
      shimPath: "C:/Torben/shims",
      targets: ["powershell"],
      newTerminalRequired: true,
    });
    const onChanged = vi.fn(async () => undefined);
    const record: InstallRecord = {
      appId: "temurin",
      version: "21.0.2+13.0.LTS",
      sourceId: "temurin.official",
      scope: "managed",
      installPath: "C:/Torben/temurin/21.0.2+13.0.LTS",
      installedAt: "fixture",
      health: "healthy",
    };

    render(
      <TemurinDetailPage
        installed={[record]}
        onChanged={onChanged}
        selected={[]}
        shellIntegration={{
          state: "disabled",
          shimPath: "C:/Torben/shims",
          targets: [],
          newTerminalRequired: false,
        }}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Set as primary version" }));

    await waitFor(() => {
      expect(select).toHaveBeenCalledWith("temurin", "21.0.2+13.0.LTS");
      expect(enableShell).toHaveBeenCalledWith(true);
      expect(onChanged).toHaveBeenCalledOnce();
    });
  });

  it("shows a quiet background-update state before the Java catalog cache is ready", async () => {
    vi.spyOn(api, "getVersions").mockResolvedValue([]);

    render(<TemurinDetailPage installed={[]} onChanged={async () => undefined} />);

    expect(
      await screen.findByText(
        "Version information is updating in the background and will appear here automatically.",
      ),
    ).toBeInTheDocument();
  });

  it("uninstalls a selected Java version after clearing its terminal selection", async () => {
    const clear = vi.spyOn(api, "clearSelection").mockResolvedValue(undefined);
    const uninstall = vi.spyOn(api, "uninstallApp").mockResolvedValue(undefined);
    const onChanged = vi.fn(async () => undefined);
    const record: InstallRecord = {
      appId: "temurin",
      version: "21.0.2+13.0.LTS",
      sourceId: "temurin.official",
      scope: "managed",
      installPath: "C:/Torben/temurin/21.0.2+13.0.LTS",
      installedAt: "fixture",
      health: "healthy",
    };

    render(<TemurinDetailPage installed={[record]} onChanged={onChanged} selected={[record]} />);

    const uninstallButton = await screen.findByRole("button", {
      name: "Uninstall Java 21.0.2+13.0.LTS",
    });
    fireEvent.click(uninstallButton);
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent("Torben App will clear the terminal selection");
    fireEvent.click(within(dialog).getByRole("button", { name: "Uninstall" }));

    await waitFor(() => {
      expect(clear).toHaveBeenCalledWith("temurin");
      expect(uninstall).toHaveBeenCalledWith("temurin", "21.0.2+13.0.LTS");
      expect(onChanged).toHaveBeenCalledOnce();
    });
    expect(clear.mock.invocationCallOrder[0]).toBeLessThan(uninstall.mock.invocationCallOrder[0]);
  });

  it("installs and selects an official Python runtime", async () => {
    vi.spyOn(api, "getVersions").mockResolvedValue([
      {
        version: "3.14.7",
        releasedAt: "2026-08-05T12:00:00Z",
        recommended: true,
      },
    ]);
    const install = vi.spyOn(api, "installApp").mockResolvedValue({} as InstallRecord);
    const onChanged = vi.fn(async () => undefined);

    const { rerender } = render(<PythonDetailPage installed={[]} onChanged={onChanged} />);
    fireEvent.click(await screen.findByRole("button", { name: "Install" }));
    await waitFor(() => expect(install).toHaveBeenCalledWith("python", "3.14.7"));

    const record: InstallRecord = {
      appId: "python",
      version: "3.14.7",
      sourceId: "python.official",
      scope: "managed",
      installPath: "C:/Torben/python/3.14.7",
      installedAt: "fixture",
      health: "healthy",
    };
    const select = vi.spyOn(api, "selectVersion").mockResolvedValue(undefined);
    rerender(<PythonDetailPage installed={[record]} onChanged={onChanged} />);
    fireEvent.click(await screen.findByRole("button", { name: "Set as primary version" }));

    await waitFor(() => expect(select).toHaveBeenCalledWith("python", "3.14.7"));
  });

  it("keeps Python patch releases independently installable", async () => {
    vi.spyOn(api, "getVersions").mockResolvedValue([
      {
        version: "3.14.8",
        releasedAt: "2026-09-01T12:00:00Z",
        recommended: true,
      },
    ]);
    const installed: InstallRecord = {
      appId: "python",
      version: "3.14.7",
      sourceId: "python.official",
      scope: "managed",
      installPath: "C:/Torben/python/3.14.7",
      installedAt: "fixture",
      health: "healthy",
    };
    const install = vi.spyOn(api, "installApp").mockResolvedValue(installed);

    render(<PythonDetailPage installed={[installed]} onChanged={async () => undefined} />);

    expect(await screen.findAllByText("Python 3.14")).toHaveLength(2);
    expect(screen.getByText("v3.14.7")).toBeInTheDocument();
    const availableRow = screen.getByText("v3.14.8").closest(".version-row");
    expect(availableRow).not.toBeNull();
    fireEvent.click(within(availableRow as HTMLElement).getByRole("button", { name: "Install" }));
    await waitFor(() => expect(install).toHaveBeenCalledWith("python", "3.14.8"));
  });

  it("restores an in-flight Python install after the detail page is remounted", async () => {
    await i18n.changeLanguage("en");
    vi.spyOn(api, "getVersions").mockResolvedValue([
      { version: "3.14.7", releasedAt: "2026-08-05T12:00:00Z", recommended: true },
    ]);
    const event: OperationEvent = {
      operationId: "33333333-3333-4333-8333-333333333333",
      sequence: 2,
      state: "running",
      phase: "install",
      message: "Extracting CPython",
      progress: 0.55,
      timestamp: "3",
      kind: "install",
      appId: "python",
      version: "3.14.7",
    };
    const first = render(
      <PythonDetailPage installed={[]} operations={[event]} onChanged={async () => undefined} />,
    );
    expect(await screen.findByRole("button", { name: "Installing…" })).toBeDisabled();
    first.unmount();
    render(
      <PythonDetailPage installed={[]} operations={[event]} onChanged={async () => undefined} />,
    );
    expect(await screen.findByRole("button", { name: "Installing…" })).toBeDisabled();
  });

  it("keeps an in-flight Python uninstall visible after the detail page is remounted", async () => {
    await i18n.changeLanguage("en");
    vi.spyOn(api, "getVersions").mockResolvedValue([
      { version: "3.14.7", releasedAt: "2026-08-05T12:00:00Z", recommended: true },
    ]);
    const record: InstallRecord = {
      appId: "python",
      version: "3.14.7",
      sourceId: "python.official",
      scope: "managed",
      installPath: "C:/Torben/python/3.14.7",
      installedAt: "fixture",
      health: "healthy",
    };
    const event: OperationEvent = {
      operationId: "44444444-4444-4444-8444-444444444444",
      sequence: 2,
      state: "running",
      phase: "uninstall",
      message: "Removing Python",
      progress: 0.4,
      timestamp: "4",
      kind: "uninstall",
      appId: "python",
      version: "3.14.7",
    };
    const first = render(
      <PythonDetailPage
        installed={[record]}
        operations={[event]}
        onChanged={async () => undefined}
      />,
    );
    expect(await screen.findByRole("button", { name: "Uninstalling…" })).toBeDisabled();
    first.unmount();
    render(
      <PythonDetailPage
        installed={[record]}
        operations={[event]}
        onChanged={async () => undefined}
      />,
    );
    expect(await screen.findByRole("button", { name: "Uninstalling…" })).toBeDisabled();
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
      currentVersion: "0.0.1",
      endpoint: "https://github.com/TorbenXiong/torben-app/releases/latest/download/latest.json",
    };
    expect(initialTorbenUpdateStatus(configuration).state).toBe("unconfigured");
    await expect(checkTorbenUpdate(configuration)).resolves.toMatchObject({
      state: "unconfigured",
      currentVersion: "0.0.1",
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

  it("shows a compact bundled Node.js card and an uninstall action", () => {
    render(
      <HashRouter>
        <PluginsPage onChanged={async () => undefined} plugins={[bundledPlugin]} />
      </HashRouter>,
    );

    expect(screen.queryByText("nodejs.org")).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: "View details for Node.js" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Uninstall Node.js" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Install plugin" }));
    expect(screen.getByText("Install a developer-mode plugin?")).toBeInTheDocument();
    expect(
      screen.getByText("Developer mode bypasses the signed official plugin registry."),
    ).toBeInTheDocument();
  });

  it("opens a separate plugin detail page with manifest information", async () => {
    const page: SchemaPage = {
      id: "runtime",
      title: "Runtime status",
      description: "Provider health",
      sections: [
        {
          id: "health",
          title: "Health",
          description: null,
          fields: [
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
          actions: [],
        },
      ],
    };
    render(
      <HashRouter>
        <PluginDetailPage plugin={bundledPlugin} loadSchemaPages={async () => [page]} />
      </HashRouter>,
    );

    expect(screen.getByRole("heading", { level: 1, name: "Node.js" })).toBeInTheDocument();
    expect(screen.getByText("Torben App")).toBeInTheDocument();
    expect(screen.getByText("nodejs.org")).toBeInTheDocument();
    expect(await screen.findByText("Runtime status")).toBeInTheDocument();
    expect(screen.getByText("Ready")).toBeInTheDocument();
  });

  it("edits environment variables for supported runtime plugins", async () => {
    const settings: UserSettings = {
      theme: "system",
      language: "system",
      updates: defaultUpdatePreferences,
      pluginOrder: [],
      applicationEnvironments: {},
    };
    const onSettingsChange = vi.fn(async () => undefined);
    render(
      <HashRouter>
        <PluginDetailPage
          loadSchemaPages={async () => []}
          onSettingsChange={onSettingsChange}
          plugin={bundledPlugin}
          settings={settings}
        />
      </HashRouter>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Add variable" }));
    fireEvent.change(screen.getByRole("combobox", { name: "Environment variable 1 name" }), {
      target: { value: "NODE_OPTIONS" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Environment variable 1 value" }), {
      target: { value: "--enable-source-maps" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save variables" }));

    await waitFor(() =>
      expect(onSettingsChange).toHaveBeenCalledWith({
        ...settings,
        applicationEnvironments: {
          node: { NODE_OPTIONS: "--enable-source-maps" },
        },
      }),
    );
  });

  it("allows installed plugins to be reordered by dragging cards", async () => {
    const secondPlugin = {
      ...bundledPlugin,
      id: "app.torben.plugin.python",
      displayName: "Python",
    };
    const onPluginOrderChange = vi.fn(async () => undefined);
    const { container } = render(
      <HashRouter>
        <Layout
          applications={[]}
          pluginOrder={[]}
          plugins={[availableTemurinPlugin, bundledPlugin, secondPlugin]}
        >
          <PluginsPage
            onChanged={async () => undefined}
            onPluginOrderChange={onPluginOrderChange}
            pluginOrder={[]}
            plugins={[availableTemurinPlugin, bundledPlugin, secondPlugin]}
          />
        </Layout>
      </HashRouter>,
    );
    const cards = () =>
      [...container.querySelectorAll(".plugin-card h2")].map((heading) => heading.textContent);
    const navigationItems = () =>
      [...container.querySelectorAll(".installed-plugin-nav a span")].map(
        (label) => label.textContent,
      );
    expect(cards()).toEqual(["Node.js", "Python", "Java"]);
    expect(navigationItems()).toEqual(["Node.js", "Python"]);
    const nodeCard = screen.getByRole("heading", { name: "Node.js" }).closest(".plugin-card");
    const pythonCard = screen.getByRole("heading", { name: "Python" }).closest(".plugin-card");
    expect(nodeCard).not.toBeNull();
    expect(pythonCard).not.toBeNull();
    fireEvent.pointerDown(screen.getByRole("button", { name: "Reorder Node.js" }), {
      button: 0,
    });
    fireEvent.pointerEnter(pythonCard as HTMLElement);
    fireEvent.pointerUp(pythonCard as HTMLElement);
    expect(cards()).toEqual(["Python", "Node.js", "Java"]);
    expect(onPluginOrderChange).toHaveBeenCalledWith([
      "app.torben.plugin.python",
      "app.torben.plugin.node",
      "app.torben.plugin.temurin",
    ]);
  });

  it("installs the available Eclipse Temurin plugin with an explicit install action", async () => {
    const installTemurin = vi.fn(async () => installedTemurinPlugin);
    const onChanged = vi.fn(async () => undefined);
    render(
      <PluginsPage
        onChanged={onChanged}
        onInstallBundledTemurin={installTemurin}
        plugins={[availableTemurinPlugin]}
      />,
    );

    expect(screen.getByText("Manage Eclipse Temurin Java runtimes")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Install Java" }));

    await waitFor(() => {
      expect(installTemurin).toHaveBeenCalledOnce();
      expect(onChanged).toHaveBeenCalledOnce();
    });
  });

  it("explains how to use and uninstall an installed Eclipse Temurin plugin", async () => {
    const uninstallTemurin = vi.fn(async () => undefined);
    const onChanged = vi.fn(async () => undefined);
    render(
      <HashRouter>
        <PluginsPage
          onChanged={onChanged}
          onUninstallBundledTemurin={uninstallTemurin}
          plugins={[installedTemurinPlugin]}
        />
      </HashRouter>,
    );

    expect(screen.getByRole("link", { name: "View details for Java" })).toHaveAttribute(
      "href",
      "#/plugins/app.torben.plugin.temurin",
    );
    expect(screen.getByText("Manage Eclipse Temurin Java runtimes")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Uninstall Java" }));
    expect(screen.getByText("Uninstall Java plugin?")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Uninstall plugin" }));

    await waitFor(() => {
      expect(uninstallTemurin).toHaveBeenCalledOnce();
      expect(onChanged).toHaveBeenCalledOnce();
    });
  });

  it("installs and uninstalls the bundled Python plugin", async () => {
    const installPython = vi.fn(async () => installedPythonPlugin);
    const uninstallPython = vi.fn(async () => undefined);
    const onChanged = vi.fn(async () => undefined);
    const { rerender } = render(
      <PluginsPage
        onChanged={onChanged}
        onInstallBundledPython={installPython}
        plugins={[availablePythonPlugin]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Install Python" }));
    await waitFor(() => expect(installPython).toHaveBeenCalledOnce());

    rerender(
      <HashRouter>
        <PluginsPage
          onChanged={onChanged}
          onUninstallBundledPython={uninstallPython}
          plugins={[installedPythonPlugin]}
        />
      </HashRouter>,
    );
    expect(screen.getByRole("link", { name: "View details for Python" })).toHaveAttribute(
      "href",
      "#/plugins/app.torben.plugin.python",
    );
    fireEvent.click(screen.getByRole("button", { name: "Uninstall Python" }));
    expect(screen.getByText("Uninstall Python plugin?")).toBeInTheDocument();
    expect(
      screen.getByText(/Any managed Python runtime must be uninstalled first/),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Uninstall plugin" }));
    await waitFor(() => expect(uninstallPython).toHaveBeenCalledOnce());
  });

  it("installs and uninstalls the bundled Node plugin", async () => {
    const installNode = vi.fn(async () => bundledPlugin);
    const uninstallNode = vi.fn(async () => undefined);
    const onChanged = vi.fn(async () => undefined);
    const { rerender } = render(
      <PluginsPage
        onChanged={onChanged}
        onInstallBundledNode={installNode}
        plugins={[{ ...bundledPlugin, enabled: false }]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Install Node.js" }));
    await waitFor(() => expect(installNode).toHaveBeenCalledOnce());

    rerender(
      <HashRouter>
        <PluginsPage
          onChanged={onChanged}
          onUninstallBundledNode={uninstallNode}
          plugins={[bundledPlugin]}
        />
      </HashRouter>,
    );
    expect(screen.getByRole("link", { name: "View details for Node.js" })).toHaveAttribute(
      "href",
      "#/plugins/app.torben.plugin.node",
    );
    fireEvent.click(screen.getByRole("button", { name: "Uninstall Node.js" }));
    expect(screen.getByText("Uninstall Node.js plugin?")).toBeInTheDocument();
    expect(screen.getByText(/Any managed Node.js must be uninstalled first/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Uninstall plugin" }));
    await waitFor(() => expect(uninstallNode).toHaveBeenCalledOnce());
  });

  it.each([
    ["Rust", "rust", "/icons/rust.svg"],
    ["MySQL", "mysql", "/icons/mysql-logo.png"],
    ["Redis", "redis", "/icons/redis-mark.svg"],
    ["PostgreSQL", "postgresql", "/icons/postgresql.svg"],
  ])("opens the %s management page and renders its own icon", (displayName, appId, iconPath) => {
    const plugin: PluginSummary = {
      ...bundledPlugin,
      id: `app.torben.plugin.${appId}`,
      displayName,
      capabilities: [...bundledPlugin.capabilities, "schema_ui"],
    };
    render(
      <HashRouter>
        <PluginsPage onChanged={async () => undefined} plugins={[plugin]} />
      </HashRouter>,
    );

    const card = screen.getByRole("heading", { name: displayName }).closest(".plugin-card");
    expect(card).not.toBeNull();
    expect(
      within(card as HTMLElement).getByRole("link", {
        name: `View details for ${displayName}`,
      }),
    ).toHaveAttribute("href", `#/plugins/app.torben.plugin.${appId}`);
    expect(card?.querySelector("img")).toHaveAttribute("src", iconPath);
    expect(within(card as HTMLElement).queryByRole("button", { name: /pages$/ })).toBeNull();
  });

  it("installs bundled database and toolchain plugins instead of toggling immutable state", async () => {
    const installRust = vi.fn(async () => ({ ...bundledPlugin, id: "app.torben.plugin.rust" }));
    const installMysql = vi.fn(async () => ({ ...bundledPlugin, id: "app.torben.plugin.mysql" }));
    const installRedis = vi.fn(async () => ({ ...bundledPlugin, id: "app.torben.plugin.redis" }));
    const installPostgresql = vi.fn(async () => ({
      ...bundledPlugin,
      id: "app.torben.plugin.postgresql",
    }));
    const onChanged = vi.fn(async () => undefined);
    const plugins = ["rust", "mysql", "redis", "postgresql"].map((appId) => ({
      ...bundledPlugin,
      id: `app.torben.plugin.${appId}`,
      displayName:
        appId === "postgresql"
          ? "PostgreSQL"
          : appId === "mysql"
            ? "MySQL"
            : appId[0].toUpperCase() + appId.slice(1),
      enabled: false,
    }));
    render(
      <HashRouter>
        <PluginsPage
          onChanged={onChanged}
          onInstallBundledRust={installRust}
          onInstallBundledMysql={installMysql}
          onInstallBundledRedis={installRedis}
          onInstallBundledPostgresql={installPostgresql}
          plugins={plugins}
        />
      </HashRouter>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Install Rust" }));
    await waitFor(() => expect(installRust).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: "Install MySQL" }));
    await waitFor(() => expect(installMysql).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: "Install Redis" }));
    await waitFor(() => expect(installRedis).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: "Install PostgreSQL" }));
    await waitFor(() => {
      expect(installPostgresql).toHaveBeenCalledOnce();
      expect(onChanged).toHaveBeenCalledTimes(4);
    });
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

  it("shows details links for official and sideloaded plugins", () => {
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

    expect(
      screen.getByRole("link", { name: "View details for Official fixture" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "View details for Fixture" })).toBeInTheDocument();
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
      <HashRouter>
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
        />
      </HashRouter>,
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

  it("uses the plugin card for details without a separate open action", () => {
    render(<PluginsPage onChanged={async () => undefined} plugins={[sideloadedPlugin]} />);

    expect(screen.getByRole("link", { name: "View details for Fixture" })).toHaveAttribute(
      "href",
      "#/plugins/dev.example.fixture",
    );
    expect(screen.queryByRole("button", { name: "Open Fixture pages" })).not.toBeInTheDocument();
  });

  it("does not install when manifest selection is cancelled", async () => {
    const chooseManifest = vi.fn(async () => null);
    const installManifest = vi.fn(async () => sideloadedPlugin);
    render(
      <HashRouter>
        <PluginsPage
          chooseManifest={chooseManifest}
          installManifest={installManifest}
          onChanged={async () => undefined}
          plugins={[bundledPlugin]}
        />
      </HashRouter>,
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
      <HashRouter>
        <PluginsPage
          chooseManifest={async () => "C:/fixture/plugin.json"}
          installManifest={installManifest}
          onChanged={onChanged}
          plugins={[bundledPlugin]}
        />
      </HashRouter>,
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
    render(<LogsPage cancel={cancel} events={events} onChanged={onChanged} />);

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
      <LogsPage
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
      pluginOrder: [],
      applicationEnvironments: {},
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
        pluginOrder: [],
        applicationEnvironments: {},
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
        settings={{ ...defaultUserSettings, language: "zh-CN" }}
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
        settings={defaultUserSettings}
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
        settings={defaultUserSettings}
        shellIntegration={disabledShellIntegration}
      />,
    );

    fireEvent.change(screen.getByRole("combobox", { name: "Torben App" }), {
      target: { value: "disabled" },
    });

    await waitFor(() => {
      expect(onChange).toHaveBeenCalledWith({
        ...defaultUserSettings,
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
        settings={defaultUserSettings}
        shellIntegration={disabledShellIntegration}
        updater={{
          configured: true,
          currentVersion: "0.0.1",
          endpoint:
            "https://github.com/TorbenXiong/torben-app/releases/latest/download/latest.json",
        }}
        updateStatus={{
          state: "available",
          currentVersion: "0.0.1",
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
        settings={defaultUserSettings}
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
        settings={defaultUserSettings}
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
