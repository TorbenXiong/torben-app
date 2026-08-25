import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import type {
  ApplicationDescriptor,
  DashboardSnapshot,
  DesktopUpdaterConfiguration,
  DoctorCheck,
  InstallRecord,
  ManagedLibraryMigrationResult,
  ManagedToPackageMigrationPlan,
  ManagedToPackageMigrationResult,
  ManagedUpdateCandidate,
  ManagedUpdateCheck,
  ManagedUpdateResult,
  OperationEvent,
  PackageToManagedMigrationPlan,
  PackageToManagedMigrationRequest,
  PackageToManagedMigrationResult,
  PluginRegistryStatus,
  PluginSummary,
  SchemaActionResult,
  SchemaPage,
  ShellIntegrationStatus,
  SourceAction,
  SourceAdapterKind,
  SourceExecutionRequest,
  SourceExecutionResult,
  SourceMigrationPlan,
  SourceMigrationRequest,
  SourceMigrationResult,
  SourceOperationPlan,
  SourcePackageKind,
  TorbenUpdateStatus,
  UserSettings,
  VersionDescriptor,
} from "./types";

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

const mockApplications: ApplicationDescriptor[] = [
  {
    id: "node",
    displayName: "Node.js",
    summary: "JavaScript runtime with managed LTS and Current releases.",
    categories: ["Runtime", "Development"],
    capabilities: ["versions", "install", "select", "uninstall", "external-detection"],
    sources: [{ id: "node.official", displayName: "Official archive", managed: true }],
  },
  {
    id: "temurin",
    displayName: "Eclipse Temurin",
    summary: "OpenJDK builds from Adoptium.",
    categories: ["Runtime", "Development"],
    capabilities: ["versions", "install", "select", "uninstall", "external-detection"],
    sources: [
      {
        id: "temurin.official",
        displayName: "Eclipse Temurin official archive",
        managed: true,
      },
    ],
  },
  {
    id: "python",
    displayName: "Python",
    summary: "The Python programming language.",
    categories: ["Runtime", "Development"],
    capabilities: ["versions", "install", "select", "uninstall", "external-detection"],
    sources: [
      { id: "python.official", displayName: "Official Python distribution", managed: true },
    ],
  },
  {
    id: "git",
    displayName: "Git",
    summary: "Official Git command-line releases with managed terminal selection.",
    categories: ["Tool", "Development"],
    capabilities: ["versions", "install", "select", "uninstall", "external-detection"],
    sources: [{ id: "git.official", displayName: "Official Git distribution", managed: true }],
  },
  {
    id: "vscode",
    displayName: "Visual Studio Code",
    summary: "Microsoft's official cross-platform code editor distribution.",
    categories: ["Editor", "Development"],
    capabilities: ["versions", "install", "select", "uninstall", "external-detection"],
    sources: [
      {
        id: "vscode.official",
        displayName: "Microsoft Visual Studio Code archive",
        managed: true,
      },
    ],
  },
  {
    id: "codex",
    displayName: "Codex CLI",
    summary: "OpenAI's official coding agent command-line client.",
    categories: ["AI", "Development"],
    capabilities: ["versions", "install", "select", "uninstall", "external-detection"],
    sources: [{ id: "codex.official", displayName: "OpenAI Codex native release", managed: true }],
  },
];

const mockNodeSchemaPages: SchemaPage[] = [
  {
    id: "node",
    title: "Node.js provider",
    description:
      "Official Node.js metadata, signed checksums, managed versions, and terminal commands.",
    sections: [
      {
        id: "trust",
        title: "Supply-chain status",
        description: "These values are declared by the bundled plugin and rendered by Torben App.",
        fields: [
          {
            id: "source",
            label: "Release source",
            description: null,
            kind: "status",
            value: "Official nodejs.org release metadata",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
          {
            id: "integrity",
            label: "Integrity",
            description: null,
            kind: "status",
            value: "OpenPGP manifest + SHA-256 archive verification",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
          {
            id: "target",
            label: "Host target",
            description: null,
            kind: "text",
            value: "preview-host",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
        ],
        actions: [],
      },
    ],
  },
];

const mockTemurinSchemaPages: SchemaPage[] = [
  {
    id: "temurin",
    title: "Eclipse Temurin provider",
    description: "Official Adoptium LTS metadata, signed archives, and managed JDK commands.",
    sections: [
      {
        id: "trust",
        title: "Supply-chain status",
        description: "Values are declared by the bundled plugin and rendered by Torben App.",
        fields: [
          {
            id: "source",
            label: "Release source",
            description: null,
            kind: "status",
            value: "Adoptium v3 Eclipse Temurin GA catalog",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
          {
            id: "integrity",
            label: "Integrity",
            description: null,
            kind: "status",
            value: "Pinned OpenPGP signer + SHA-256 archive verification",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
        ],
        actions: [],
      },
    ],
  },
];

const mockPythonSchemaPages: SchemaPage[] = [
  {
    id: "python",
    title: "Python provider",
    description: "Official stable CPython metadata, managed runtimes, pip, and terminal commands.",
    sections: [
      {
        id: "trust",
        title: "Supply-chain status",
        description: "Values are declared by the bundled plugin and rendered by Torben App.",
        fields: [
          {
            id: "source",
            label: "Release source",
            description: null,
            kind: "status",
            value: "python.org official release catalog",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
          {
            id: "integrity",
            label: "Integrity",
            description: null,
            kind: "status",
            value: "Python Install Manager signed catalog or Sigstore + SHA-256",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
        ],
        actions: [],
      },
    ],
  },
];

const mockGitSchemaPages: SchemaPage[] = [
  {
    id: "git",
    title: "Git provider",
    description: "Official Git CLI metadata, transactional installation, and terminal selection.",
    sections: [
      {
        id: "trust",
        title: "Supply-chain status",
        description: "Values are declared by the bundled plugin and rendered by Torben App.",
        fields: [
          {
            id: "source",
            label: "Release source",
            description: null,
            kind: "status",
            value: "git-for-windows or kernel.org stable catalog",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
          {
            id: "integrity",
            label: "Integrity",
            description: null,
            kind: "status",
            value: "Pinned official SHA-256 metadata",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
        ],
        actions: [],
      },
    ],
  },
];

const mockVsCodeSchemaPages: SchemaPage[] = [
  {
    id: "vscode",
    title: "Visual Studio Code provider",
    description:
      "Published stable releases, Microsoft SHA-256 metadata, and the managed code command.",
    sections: [
      {
        id: "trust",
        title: "Supply-chain status",
        description:
          "User settings, accounts, extensions, credentials, and projects remain external.",
        fields: [
          {
            id: "source",
            label: "Release source",
            description: null,
            kind: "status",
            value: "Published microsoft/vscode release + Microsoft Update metadata",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
          {
            id: "integrity",
            label: "Integrity",
            description: null,
            kind: "status",
            value: "Microsoft SHA-256 + exact commit",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
          {
            id: "updates",
            label: "Managed launch",
            description: null,
            kind: "status",
            value: "code --disable-updates",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
        ],
        actions: [],
      },
    ],
  },
];

const mockCodexSchemaPages: SchemaPage[] = [
  {
    id: "codex",
    title: "Codex CLI provider",
    description: "Official native Codex releases, exact versions, and the managed codex command.",
    sections: [
      {
        id: "trust",
        title: "Supply-chain and identity boundary",
        description:
          "Torben manages executable versions only and never opens Codex authentication or configuration data.",
        fields: [
          {
            id: "source",
            label: "Release source",
            description: null,
            kind: "status",
            value: "openai/codex stable native release",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
          {
            id: "integrity",
            label: "Integrity",
            description: null,
            kind: "status",
            value: "GitHub SHA-256 and platform Sigstore where published",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
          {
            id: "identity",
            label: "Authentication",
            description: null,
            kind: "status",
            value: "External: CODEX_HOME, auth.json, keyring, and login state",
            placeholder: null,
            options: [],
            readOnly: true,
            required: false,
          },
        ],
        actions: [],
      },
    ],
  },
];

const mockSnapshot: DashboardSnapshot = {
  applications: mockApplications,
  installed: [
    {
      appId: "node",
      version: "24.19.0",
      sourceId: "node.official",
      scope: "managed",
      installPath: "mock/data/apps/node/24.19.0",
      installedAt: "2026-08-20T00:00:00Z",
      health: "healthy",
    },
  ],
  selected: [{ appId: "node", version: "24.19.0" }],
  external: [],
  warnings: [],
  operations: [],
  plugins: [
    {
      id: "app.torben.plugin.node",
      displayName: "Node.js",
      version: "0.1.0",
      enabled: true,
      origin: "built_in",
      publisher: "Torben App",
      capabilities: [
        "version_discovery",
        "external_discovery",
        "managed_install",
        "global_selection",
        "managed_uninstall",
        "schema_ui",
      ],
      permissions: {
        networkDomains: ["nodejs.org"],
        filesystemRoots: ["managed_app_library", "download_cache", "staging"],
        externalCommands: ["node", "npm", "npx"],
        packageManagers: [],
      },
    },
    {
      id: "app.torben.plugin.temurin",
      displayName: "Eclipse Temurin",
      version: "0.1.0",
      enabled: true,
      origin: "built_in",
      publisher: "Torben App",
      capabilities: [
        "version_discovery",
        "external_discovery",
        "managed_install",
        "global_selection",
        "managed_uninstall",
        "schema_ui",
      ],
      permissions: {
        networkDomains: [
          "api.adoptium.net",
          "github.com",
          "release-assets.githubusercontent.com",
          "packages.adoptium.net",
        ],
        filesystemRoots: ["managed_app_library", "download_cache", "staging"],
        externalCommands: ["java", "javac"],
        packageManagers: [],
      },
    },
    {
      id: "app.torben.plugin.python",
      displayName: "Python",
      version: "0.1.0",
      enabled: true,
      origin: "built_in",
      publisher: "Torben App",
      capabilities: [
        "version_discovery",
        "external_discovery",
        "managed_install",
        "global_selection",
        "managed_uninstall",
        "schema_ui",
      ],
      permissions: {
        networkDomains: ["www.python.org"],
        filesystemRoots: ["managed_app_library", "download_cache", "staging"],
        externalCommands: ["py", "make", "cc", "python", "python3", "pip", "pip3"],
        packageManagers: [],
      },
    },
    {
      id: "app.torben.plugin.git",
      displayName: "Git",
      version: "0.1.0",
      enabled: true,
      origin: "built_in",
      publisher: "Torben App",
      capabilities: [
        "version_discovery",
        "external_discovery",
        "managed_install",
        "global_selection",
        "managed_uninstall",
        "schema_ui",
      ],
      permissions: {
        networkDomains: [
          "api.github.com",
          "github.com",
          "release-assets.githubusercontent.com",
          "www.kernel.org",
        ],
        filesystemRoots: ["managed_app_library", "download_cache", "staging"],
        externalCommands: ["make", "cc", "git"],
        packageManagers: [],
      },
    },
    {
      id: "app.torben.plugin.vscode",
      displayName: "Visual Studio Code",
      version: "0.1.0",
      enabled: true,
      origin: "built_in",
      publisher: "Torben App",
      capabilities: [
        "version_discovery",
        "external_discovery",
        "managed_install",
        "global_selection",
        "managed_uninstall",
        "schema_ui",
      ],
      permissions: {
        networkDomains: [
          "api.github.com",
          "update.code.visualstudio.com",
          "vscode.download.prss.microsoft.com",
        ],
        filesystemRoots: ["managed_app_library", "download_cache", "staging"],
        externalCommands: ["code"],
        packageManagers: [],
      },
    },
    {
      id: "app.torben.plugin.codex",
      displayName: "Codex CLI",
      version: "0.1.0",
      enabled: true,
      origin: "built_in",
      publisher: "Torben App",
      capabilities: [
        "version_discovery",
        "external_discovery",
        "managed_install",
        "global_selection",
        "managed_uninstall",
        "schema_ui",
      ],
      permissions: {
        networkDomains: ["api.github.com", "github.com", "release-assets.githubusercontent.com"],
        filesystemRoots: ["managed_app_library", "download_cache", "staging"],
        externalCommands: ["codex"],
        packageManagers: [],
      },
    },
  ],
  pluginRegistry: {
    configured: false,
    sourceUrl: null,
    cachePath: "mock/cache/plugin-registry/official/registry.json",
    sequence: null,
    generatedAt: null,
  },
  doctor: [
    { id: "data_directory", healthy: true, message: "Platform data directory ready" },
    { id: "diagnostic_log", healthy: true, message: "Platform diagnostic log ready" },
    { id: "shell_integration", healthy: false, message: "User PATH integration is disabled" },
  ],
  sourceAdapters: [
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
      adapter: "homebrew",
      sourceId: "source.homebrew",
      availability: "unsupported",
      executable: null,
      version: null,
      supportsExactVersion: false,
      requiresElevation: false,
      message: "This adapter is not supported on the current operating system.",
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
    {
      adapter: "dnf",
      sourceId: "source.dnf",
      availability: "unsupported",
      executable: null,
      version: null,
      supportsExactVersion: true,
      requiresElevation: true,
      message: "This adapter is not supported on the current operating system.",
    },
  ],
  packageInstallations: [],
  updater: {
    configured: false,
    currentVersion: "0.1.0",
    endpoint: "https://github.com/TorbenXiong/torben-app/releases/latest/download/latest.json",
  },
  settings: {
    theme: "system",
    language: "system",
    updates: {
      notifyTorbenApp: true,
      notifyManagedApps: true,
      automaticallyInstallTorbenApp: false,
      automaticallyUpdateApps: [],
    },
  },
  shellIntegration: {
    state: "disabled",
    shimPath: "Platform data directory/tools/shims",
    targets: [],
    newTerminalRequired: false,
  },
  managedLibrary: {
    path: "Platform data directory/apps",
    defaultPath: "Platform data directory/apps",
    custom: false,
    bytesUsed: 0,
  },
};

function isTauri() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

let pendingTorbenUpdate: Update | null = null;

export function initialTorbenUpdateStatus(
  configuration: DesktopUpdaterConfiguration,
): TorbenUpdateStatus {
  return {
    state: configuration.configured ? "idle" : "unconfigured",
    currentVersion: configuration.currentVersion,
    availableVersion: null,
    publishedAt: null,
    notes: null,
    progress: null,
    message: null,
  };
}

export async function checkTorbenUpdate(
  configuration: DesktopUpdaterConfiguration,
): Promise<TorbenUpdateStatus> {
  if (!configuration.configured) {
    return initialTorbenUpdateStatus(configuration);
  }
  if (!isTauri()) {
    return {
      ...initialTorbenUpdateStatus(configuration),
      state: "up_to_date",
      message: "Mock desktop is up to date.",
    };
  }
  if (pendingTorbenUpdate) {
    await pendingTorbenUpdate.close();
    pendingTorbenUpdate = null;
  }
  const update = await check({ timeout: 15_000 });
  if (!update) {
    return {
      ...initialTorbenUpdateStatus(configuration),
      state: "up_to_date",
      message: "Torben App is up to date.",
    };
  }
  pendingTorbenUpdate = update;
  return {
    state: "available",
    currentVersion: update.currentVersion,
    availableVersion: update.version,
    publishedAt: update.date ?? null,
    notes: update.body ?? null,
    progress: null,
    message: `Torben App ${update.version} is available.`,
  };
}

export async function installTorbenUpdate(
  configuration: DesktopUpdaterConfiguration,
  onProgress: (progress: number | null) => void,
): Promise<void> {
  if (!configuration.configured || !isTauri()) {
    throw {
      code: "updater_unconfigured",
      message: "This build cannot install updates because no trusted updater key is configured.",
      remediation: "Install a signed official build or configure the reviewed updater key.",
    };
  }
  if (!pendingTorbenUpdate) {
    throw {
      code: "update_not_checked",
      message: "No verified update is pending installation.",
      remediation: "Check for updates again before installing.",
    };
  }
  let downloaded = 0;
  let total: number | undefined;
  await pendingTorbenUpdate.downloadAndInstall((event) => {
    if (event.event === "Started") {
      total = event.data.contentLength;
      onProgress(total ? 0 : null);
    } else if (event.event === "Progress") {
      downloaded += event.data.chunkLength;
      onProgress(total ? Math.min(1, downloaded / total) : null);
    } else {
      onProgress(1);
    }
  });
  await pendingTorbenUpdate.close();
  pendingTorbenUpdate = null;
  await relaunch();
}

export function formatTorbenError(reason: unknown): string {
  if (reason instanceof Error) {
    return reason.message;
  }
  if (reason && typeof reason === "object") {
    const payload = reason as Record<string, unknown>;
    if (typeof payload.message === "string") {
      const code = typeof payload.code === "string" ? `[${payload.code}] ` : "";
      const remediation = typeof payload.remediation === "string" ? ` ${payload.remediation}` : "";
      return `${code}${payload.message}${remediation}`;
    }
  }
  return String(reason);
}

export async function getSnapshot(): Promise<DashboardSnapshot> {
  return isTauri() ? invoke<DashboardSnapshot>("dashboard_snapshot") : mockSnapshot;
}

export async function getVersions(appId: string): Promise<VersionDescriptor[]> {
  if (isTauri()) {
    return invoke<VersionDescriptor[]>("list_versions", { appId });
  }
  if (appId === "python") {
    return [
      { version: "3.14.7", releasedAt: "2026-08-05T12:00:00Z", recommended: true },
      { version: "3.13.15", releasedAt: "2026-08-05T11:00:00Z", recommended: false },
    ];
  }
  if (appId === "git") {
    return [
      {
        version: "2.55.0+windows.5",
        releasedAt: "2026-08-20T16:21:31Z",
        recommended: true,
      },
    ];
  }
  if (appId === "vscode") {
    return [
      { version: "1.134.0", releasedAt: "2026-08-19T09:08:11Z", recommended: true },
      { version: "1.133.0", releasedAt: "2026-08-12T09:41:17Z", recommended: false },
    ];
  }
  if (appId === "codex") {
    return [
      { version: "0.149.1", releasedAt: "2026-08-24T00:28:28Z", recommended: true },
      { version: "0.149.0", releasedAt: "2026-08-20T21:04:55Z", recommended: false },
    ];
  }
  return appId === "temurin"
    ? [
        {
          version: "21.0.2+13.0.LTS",
          ltsName: "Java 21 LTS",
          releasedAt: "2026-01-20T00:00:00Z",
          recommended: true,
        },
      ]
    : [
        { version: "24.19.0", ltsName: "Krypton", releasedAt: "2026-08-03", recommended: true },
        { version: "22.22.3", ltsName: "Jod", releasedAt: "2026-05-20", recommended: true },
        { version: "26.7.0", releasedAt: "2026-08-05", recommended: false },
      ];
}

export async function installApp(appId: string, version: string): Promise<InstallRecord> {
  if (!isTauri()) {
    throw new Error("Installation is available in the Tauri desktop runtime.");
  }
  return invoke<InstallRecord>("install_app", { appId, version });
}

export async function selectVersion(appId: string, version: string): Promise<void> {
  if (!isTauri()) {
    throw new Error("Version selection is available in the Tauri desktop runtime.");
  }
  await invoke("select_version", { appId, version });
}

export async function clearSelection(appId: string): Promise<void> {
  if (!isTauri()) {
    throw new Error("Clearing a selection is available in the Tauri desktop runtime.");
  }
  await invoke("clear_selection", { appId });
}

export async function uninstallApp(appId: string, version: string): Promise<void> {
  if (!isTauri()) {
    throw new Error("Uninstall is available in the Tauri desktop runtime.");
  }
  await invoke("uninstall_app", { appId, version });
}

export async function checkManagedUpdates(appId?: string): Promise<ManagedUpdateCheck> {
  if (isTauri()) {
    return invoke<ManagedUpdateCheck>("check_managed_updates", { appId });
  }
  if (appId && appId !== "node") {
    return { checkedApps: 0, candidates: [], warnings: [] };
  }
  return {
    checkedApps: 1,
    candidates: [
      {
        appId: "node",
        channel: "24",
        installedVersion: "24.19.0",
        availableVersion: "24.20.1",
        selectedVersion: "24.19.0",
        releasedAt: "2026-08-24T00:00:00Z",
        recommended: true,
        automatic: mockSnapshot.settings.updates.automaticallyUpdateApps.includes("node"),
      },
    ],
    warnings: [],
  };
}

export async function applyManagedUpdate(
  candidate: ManagedUpdateCandidate,
): Promise<ManagedUpdateResult> {
  if (!isTauri()) {
    throw new Error("Managed application updates are available in the Tauri desktop runtime.");
  }
  return invoke<ManagedUpdateResult>("apply_managed_update", {
    appId: candidate.appId,
    installedVersion: candidate.installedVersion,
    availableVersion: candidate.availableVersion,
  });
}

export async function setManagedAutoUpdate(appId: string, enabled: boolean): Promise<UserSettings> {
  if (isTauri()) {
    return invoke<UserSettings>("set_managed_auto_update", { appId, enabled });
  }
  const current = new Set(mockSnapshot.settings.updates.automaticallyUpdateApps);
  if (enabled) current.add(appId);
  else current.delete(appId);
  mockSnapshot.settings = {
    ...mockSnapshot.settings,
    updates: {
      ...mockSnapshot.settings.updates,
      automaticallyUpdateApps: Array.from(current).sort(),
    },
  };
  return mockSnapshot.settings;
}

export async function runDoctor(): Promise<DoctorCheck[]> {
  return isTauri() ? invoke<DoctorCheck[]>("run_doctor") : mockSnapshot.doctor;
}

export async function planSourceOperation(
  action: SourceAction,
  adapter: SourceAdapterKind,
  coordinate: string,
  packageKind: SourcePackageKind,
  packageVersion: string | null,
): Promise<SourceOperationPlan> {
  if (isTauri()) {
    return invoke<SourceOperationPlan>("plan_source_operation", {
      action,
      adapter,
      package: coordinate,
      packageKind,
      packageVersion,
    });
  }
  const operation = action === "install" ? "install" : "uninstall";
  return {
    action,
    adapter,
    sourceId: `source.${adapter}`,
    coordinate,
    packageKind,
    packageVersion,
    executable: adapter === "winget" ? "winget.exe" : adapter,
    previewArguments: ["show", coordinate],
    executeArguments: [operation, coordinate],
    executionIdentity:
      adapter === "dnf" && packageVersion ? `${coordinate}-${packageVersion}.x86_64` : null,
    environment: {},
    requiresElevation: adapter === "apt" || adapter === "dnf",
    exactVersionGuaranteed:
      adapter !== "homebrew" && (action === "uninstall" || Boolean(packageVersion)),
    mutatesSystem: true,
    warnings: [
      "Preview only: execution requires the Tauri desktop runtime and explicit acceptance.",
    ],
  };
}

export async function executeSourceOperation(
  request: SourceExecutionRequest,
): Promise<SourceExecutionResult> {
  if (!isTauri()) {
    throw new Error("Package-manager execution is available in the Tauri desktop runtime.");
  }
  return invoke<SourceExecutionResult>("execute_source_operation", { request });
}

export async function planSourceMigration(
  request: SourceMigrationRequest,
): Promise<SourceMigrationPlan> {
  if (!isTauri()) {
    throw new Error("Source migration planning is available in the Tauri desktop runtime.");
  }
  return invoke<SourceMigrationPlan>("plan_source_migration", { request });
}

export async function executeSourceMigration(
  request: SourceMigrationRequest,
): Promise<SourceMigrationResult> {
  if (!isTauri()) {
    throw new Error("Source migration is available in the Tauri desktop runtime.");
  }
  return invoke<SourceMigrationResult>("execute_source_migration", { request });
}

export async function planManagedToPackageMigration(
  request: SourceMigrationRequest,
): Promise<ManagedToPackageMigrationPlan> {
  if (!isTauri()) {
    throw new Error("Managed-to-package migration planning requires the Tauri desktop runtime.");
  }
  return invoke<ManagedToPackageMigrationPlan>("plan_managed_to_package_migration", { request });
}

export async function executeManagedToPackageMigration(
  request: SourceMigrationRequest,
): Promise<ManagedToPackageMigrationResult> {
  if (!isTauri()) {
    throw new Error("Managed-to-package migration requires the Tauri desktop runtime.");
  }
  return invoke<ManagedToPackageMigrationResult>("execute_managed_to_package_migration", {
    request,
  });
}

export async function planPackageToManagedMigration(
  request: PackageToManagedMigrationRequest,
): Promise<PackageToManagedMigrationPlan> {
  if (!isTauri()) {
    throw new Error("Package-to-managed migration planning requires the Tauri desktop runtime.");
  }
  return invoke<PackageToManagedMigrationPlan>("plan_package_to_managed_migration", { request });
}

export async function executePackageToManagedMigration(
  request: PackageToManagedMigrationRequest,
): Promise<PackageToManagedMigrationResult> {
  if (!isTauri()) {
    throw new Error("Package-to-managed migration requires the Tauri desktop runtime.");
  }
  return invoke<PackageToManagedMigrationResult>("execute_package_to_managed_migration", {
    request,
  });
}

export async function getOperationEvents(): Promise<OperationEvent[]> {
  return isTauri() ? invoke<OperationEvent[]>("list_operations") : mockSnapshot.operations;
}

export async function cancelOperation(operationId: string): Promise<void> {
  if (!isTauri()) {
    throw new Error("Operation cancellation is available in the Tauri desktop runtime.");
  }
  await invoke("cancel_operation", { operationId });
}

export async function updateSettings(settings: UserSettings): Promise<void> {
  if (isTauri()) {
    await invoke("update_settings", { settings });
    return;
  }
  mockSnapshot.settings = settings;
}

export async function setShellIntegration(enabled: boolean): Promise<ShellIntegrationStatus> {
  if (isTauri()) {
    return invoke<ShellIntegrationStatus>("set_shell_integration", { enabled });
  }
  const state = enabled ? "managed" : "disabled";
  mockSnapshot.shellIntegration = {
    ...mockSnapshot.shellIntegration,
    state,
    newTerminalRequired: mockSnapshot.shellIntegration.state !== state,
  };
  return mockSnapshot.shellIntegration;
}

export async function migrateManagedLibrary(
  targetPath: string,
): Promise<ManagedLibraryMigrationResult> {
  if (!isTauri()) {
    throw new Error("Library migration is available in the Tauri desktop runtime.");
  }
  return invoke<ManagedLibraryMigrationResult>("migrate_managed_library", { targetPath });
}

export async function installPlugin(
  manifestPath: string,
  developerMode: boolean,
): Promise<PluginSummary> {
  if (!isTauri()) {
    throw new Error("Plugin installation is available in the Tauri desktop runtime.");
  }
  return invoke<PluginSummary>("install_plugin", { manifestPath, developerMode });
}

export async function installOfficialPlugin(
  registryPath: string,
  pluginId: string,
  version?: string,
): Promise<PluginSummary> {
  if (!isTauri()) {
    throw new Error("Official plugin installation is available in the Tauri desktop runtime.");
  }
  return invoke<PluginSummary>("install_official_plugin", { registryPath, pluginId, version });
}

export async function installOfficialPluginFromRegistry(
  pluginId: string,
  version?: string,
): Promise<PluginSummary> {
  if (!isTauri()) {
    throw new Error("Official plugin installation is available in the Tauri desktop runtime.");
  }
  return invoke<PluginSummary>("install_official_plugin_from_registry", { pluginId, version });
}

export async function getOfficialPluginRegistryStatus(): Promise<PluginRegistryStatus> {
  if (!isTauri()) {
    throw new Error("Official plugin registry status is available in the Tauri desktop runtime.");
  }
  return invoke<PluginRegistryStatus>("official_plugin_registry_status");
}

export async function refreshOfficialPluginRegistry(): Promise<PluginRegistryStatus> {
  if (!isTauri()) {
    throw new Error("Official plugin registry refresh is available in the Tauri desktop runtime.");
  }
  return invoke<PluginRegistryStatus>("refresh_official_plugin_registry");
}

export async function setPluginEnabled(pluginId: string, enabled: boolean): Promise<void> {
  if (!isTauri()) {
    throw new Error("Plugin state changes are available in the Tauri desktop runtime.");
  }
  await invoke("set_plugin_enabled", { pluginId, enabled });
}

export async function getPluginSchemaPages(pluginId: string): Promise<SchemaPage[]> {
  if (!isTauri()) {
    if (pluginId === "app.torben.plugin.node") return mockNodeSchemaPages;
    if (pluginId === "app.torben.plugin.temurin") return mockTemurinSchemaPages;
    if (pluginId === "app.torben.plugin.python") return mockPythonSchemaPages;
    if (pluginId === "app.torben.plugin.git") return mockGitSchemaPages;
    if (pluginId === "app.torben.plugin.vscode") return mockVsCodeSchemaPages;
    if (pluginId === "app.torben.plugin.codex") return mockCodexSchemaPages;
    return [];
  }
  return invoke<SchemaPage[]>("plugin_schema_pages", { pluginId });
}

export async function invokePluginSchemaAction(
  pluginId: string,
  pageId: string,
  sectionId: string,
  actionId: string,
  values: Record<string, string>,
  confirmed: boolean,
): Promise<SchemaActionResult> {
  if (!isTauri()) {
    throw new Error(`Schema action ${actionId} is unavailable in the web preview for ${pluginId}.`);
  }
  return invoke<SchemaActionResult>("invoke_plugin_schema_action", {
    pluginId,
    pageId,
    sectionId,
    actionId,
    values,
    confirmed,
  });
}
