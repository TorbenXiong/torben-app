export interface InstallSource {
  id: string;
  displayName: string;
  managed: boolean;
}

export interface ApplicationDescriptor {
  id: string;
  displayName: string;
  summary: string;
  categories: string[];
  capabilities: string[];
  sources: InstallSource[];
}

export interface VersionDescriptor {
  version: string;
  ltsName?: string;
  releasedAt: string;
  recommended: boolean;
}

export interface InstallRecord {
  appId: string;
  version: string;
  sourceId: string;
  scope: "managed" | "external" | "package_manager";
  installPath: string;
  installedAt: string;
  health: string;
}

export interface SelectionRecord {
  appId: string;
  version: string;
}

export interface OperationEvent {
  operationId: string;
  sequence: number;
  state: "pending" | "running" | "cancelling" | "succeeded" | "failed" | "rolled_back";
  phase: string;
  message: string;
  progress?: number;
  timestamp: string;
}

export interface DoctorCheck {
  id: string;
  healthy: boolean;
  message: string;
}

export type SourceAdapterKind = "winget" | "homebrew" | "apt" | "dnf";
export type SourceAdapterAvailability = "unsupported" | "missing" | "available";

export interface SourceAdapterStatus {
  adapter: SourceAdapterKind;
  sourceId: string;
  availability: SourceAdapterAvailability;
  executable: string | null;
  version: string | null;
  supportsExactVersion: boolean;
  requiresElevation: boolean;
  message: string;
}

export type SourceAction = "install" | "uninstall";
export type SourcePackageKind = "native" | "formula" | "cask";

export interface SourcePackageState {
  adapter: SourceAdapterKind;
  sourceId: string;
  coordinate: string;
  packageKind: SourcePackageKind;
  installed: boolean;
  installedVersion: string | null;
  architecture: string | null;
  managerOwned: boolean;
}

export interface SourceOperationPlan {
  action: SourceAction;
  adapter: SourceAdapterKind;
  sourceId: string;
  coordinate: string;
  packageKind: SourcePackageKind;
  packageVersion: string | null;
  executable: string;
  previewArguments: string[];
  executeArguments: string[];
  executionIdentity: string | null;
  environment: Record<string, string>;
  requiresElevation: boolean;
  exactVersionGuaranteed: boolean;
  mutatesSystem: boolean;
  warnings: string[];
}

export interface PackageInstallationRecord {
  appId: string;
  appVersion: string;
  sourceId: string;
  adapter: SourceAdapterKind;
  coordinate: string;
  packageKind: SourcePackageKind;
  packageVersion: string;
  architecture: string;
  executablePath: string;
  ownedByTorben: boolean;
  installedAt: string;
  health: string;
}

export interface SourceExecutionRequest {
  appId: string;
  appVersion: string;
  action: SourceAction;
  adapter: SourceAdapterKind;
  coordinate: string;
  packageKind: SourcePackageKind;
  packageVersion: string | null;
  executablePath: string | null;
  approvedExecutionIdentity: string | null;
  acceptSystemChanges: boolean;
}

export interface SourceExecutionResult {
  operationId: string;
  plan: SourceOperationPlan;
  before: SourcePackageState;
  after: SourcePackageState;
  outcome: "ownership_committed" | "ownership_removed";
  installation: PackageInstallationRecord | null;
}

export interface SourceMigrationRequest {
  appId: string;
  appVersion: string;
  targetAdapter: SourceAdapterKind;
  targetCoordinate: string;
  targetPackageKind: SourcePackageKind;
  targetPackageVersion: string | null;
  targetExecutablePath: string;
  approvedPlanToken: string | null;
  acceptSystemChanges: boolean;
}

export interface SourceMigrationPlan {
  appId: string;
  appVersion: string;
  currentOwner: PackageInstallationRecord;
  currentState: SourcePackageState;
  targetState: SourcePackageState;
  uninstallCurrent: SourceOperationPlan;
  installTarget: SourceOperationPlan;
  cleanupTarget: SourceOperationPlan;
  restoreCurrent: SourceOperationPlan;
  targetExecutablePath: string;
  approvalToken: string;
  warnings: string[];
}

export interface SourceMigrationResult {
  operationId: string;
  plan: SourceMigrationPlan;
  installation: PackageInstallationRecord;
}

export interface ManagedToPackageMigrationPlan {
  appId: string;
  appVersion: string;
  currentInstallation: InstallRecord;
  uninstallCurrent: {
    appId: string;
    version: string;
    sourceId: string;
    installPath: string;
    preserveUserData: boolean;
  };
  targetState: SourcePackageState;
  installTarget: SourceOperationPlan;
  cleanupTarget: SourceOperationPlan;
  targetExecutablePath: string;
  approvalToken: string;
  warnings: string[];
}

export interface ManagedToPackageMigrationResult {
  operationId: string;
  plan: ManagedToPackageMigrationPlan;
  installation: PackageInstallationRecord;
}

export interface PackageToManagedMigrationRequest {
  appId: string;
  appVersion: string;
  approvedPlanToken: string | null;
  acceptSystemChanges: boolean;
}

export interface PackageToManagedMigrationPlan {
  appId: string;
  appVersion: string;
  currentOwner: PackageInstallationRecord;
  currentState: SourcePackageState;
  uninstallCurrent: SourceOperationPlan;
  restoreCurrent: SourceOperationPlan;
  installManaged: {
    appId: string;
    version: string;
    sourceId: string;
    steps: Array<{ type: string; [key: string]: unknown }>;
    metadata: Record<string, string>;
  };
  managedTargetPath: string;
  approvalToken: string;
  warnings: string[];
}

export interface PackageToManagedMigrationResult {
  operationId: string;
  plan: PackageToManagedMigrationPlan;
  installation: InstallRecord;
}

export type PluginCapability =
  | "version_discovery"
  | "external_discovery"
  | "managed_install"
  | "global_selection"
  | "managed_uninstall"
  | "schema_ui";

export interface PluginPermissions {
  networkDomains: string[];
  filesystemRoots: string[];
  externalCommands: string[];
  packageManagers: string[];
}

export interface PluginSummary {
  id: string;
  displayName: string;
  version: string;
  enabled: boolean;
  origin: "built_in" | "official_registry" | "sideloaded";
  publisher: string;
  capabilities: PluginCapability[];
  permissions: PluginPermissions;
}

export interface PluginRegistryStatus {
  configured: boolean;
  sourceUrl: string | null;
  cachePath: string;
  sequence: number | null;
  generatedAt: string | null;
}

export type SchemaFieldKind = "text" | "boolean" | "select" | "status";
export type SchemaActionKind = "primary" | "secondary" | "destructive";

export interface SchemaOption {
  value: string;
  label: string;
}

export interface SchemaField {
  id: string;
  label: string;
  description: string | null;
  kind: SchemaFieldKind;
  value: string | null;
  placeholder: string | null;
  options: SchemaOption[];
  readOnly: boolean;
  required: boolean;
}

export interface SchemaAction {
  id: string;
  label: string;
  description: string | null;
  kind: SchemaActionKind;
  enabled: boolean;
  confirmation: string | null;
}

export interface SchemaSection {
  id: string;
  title: string | null;
  description: string | null;
  fields: SchemaField[];
  actions: SchemaAction[];
}

export interface SchemaPage {
  id: string;
  title: string;
  description: string | null;
  sections: SchemaSection[];
}

export interface SchemaActionResult {
  pluginId: string;
  page: SchemaPage;
  message: string | null;
}

export interface UserSettings {
  theme: "system" | "light" | "dark";
  language: "system" | "en" | "zh-CN";
  updates: UpdatePreferences;
}

export interface UpdatePreferences {
  notifyTorbenApp: boolean;
  notifyManagedApps: boolean;
  automaticallyInstallTorbenApp: boolean;
  automaticallyUpdateApps: string[];
}

export interface DesktopUpdaterConfiguration {
  configured: boolean;
  currentVersion: string;
  endpoint: string;
}

export interface TorbenUpdateStatus {
  state: "unconfigured" | "idle" | "checking" | "up_to_date" | "available" | "installing" | "error";
  currentVersion: string;
  availableVersion: string | null;
  publishedAt: string | null;
  notes: string | null;
  progress: number | null;
  message: string | null;
}

export interface ShellIntegrationStatus {
  state: "disabled" | "managed" | "external" | "outdated";
  shimPath: string;
  targets: string[];
  newTerminalRequired: boolean;
}

export interface ManagedLibraryStatus {
  path: string;
  defaultPath: string;
  custom: boolean;
  bytesUsed: number;
}

export interface ManagedLibraryMigrationResult {
  previousPath: string;
  currentPath: string;
  bytesCopied: number;
  sourceCleanupPending: boolean;
}

export interface ManagedUpdateCandidate {
  appId: string;
  channel: string;
  installedVersion: string;
  availableVersion: string;
  selectedVersion: string | null;
  releasedAt: string;
  recommended: boolean;
  automatic: boolean;
}

export interface ManagedUpdateWarning {
  appId: string;
  code: string;
  message: string;
  details: Record<string, string>;
  remediation: string | null;
}

export interface ManagedUpdateCheck {
  checkedApps: number;
  candidates: ManagedUpdateCandidate[];
  warnings: ManagedUpdateWarning[];
}

export interface ManagedUpdateResult {
  candidate: ManagedUpdateCandidate;
  installation: InstallRecord;
  selectionUpdated: boolean;
}

export interface DashboardWarning {
  appId: string;
  code: string;
  message: string;
  details: Record<string, string>;
  remediation: string | null;
}

export interface DashboardSnapshot {
  applications: ApplicationDescriptor[];
  installed: InstallRecord[];
  selected: SelectionRecord[];
  external: InstallRecord[];
  warnings: DashboardWarning[];
  operations: OperationEvent[];
  plugins: PluginSummary[];
  pluginRegistry: PluginRegistryStatus;
  doctor: DoctorCheck[];
  sourceAdapters: SourceAdapterStatus[];
  packageInstallations: PackageInstallationRecord[];
  updater: DesktopUpdaterConfiguration;
  settings: UserSettings;
  shellIntegration: ShellIntegrationStatus;
  managedLibrary: ManagedLibraryStatus;
}
