mod envelope;
mod error;
mod model;
pub mod plugin;
mod settings;
mod shell;
mod source;
mod storage;
mod updates;

pub use envelope::{API_SCHEMA_VERSION, ApiEnvelope};
pub use error::{TorbenError, TorbenResult};
pub use model::{
    AppId, ApplicationDescriptor, ExactVersion, InstallRecord, InstallScope, InstallSource,
    OperationEvent, OperationId, OperationKind, OperationState, PluginId, SelectionRecord,
    SourceId, VersionDescriptor,
};
pub use settings::{LanguagePreference, ThemePreference, UpdatePreferences, UserSettings};
pub use shell::{ShellIntegrationState, ShellIntegrationStatus};
pub use source::{
    ManagedToPackageMigrationPlan, ManagedToPackageMigrationResult, PackageCoordinate,
    PackageInstallationRecord, PackageToManagedMigrationPlan, PackageToManagedMigrationRequest,
    PackageToManagedMigrationResult, SourceAction, SourceAdapterAvailability, SourceAdapterKind,
    SourceAdapterStatus, SourceExecutionOutcome, SourceExecutionRequest, SourceExecutionResult,
    SourceMigrationPlan, SourceMigrationRequest, SourceMigrationResult, SourceOperationPlan,
    SourcePackageKind, SourcePackageState, SourcePackageVersion,
};
pub use storage::{ManagedLibraryMigrationResult, ManagedLibraryStatus};
pub use updates::{
    ManagedUpdateCandidate, ManagedUpdateCheck, ManagedUpdateResult, ManagedUpdateWarning,
};
