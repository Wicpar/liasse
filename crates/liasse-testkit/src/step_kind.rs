//! The step action vocabulary and its scoping rule.
//!
//! FORMAT.md fixes a set of built-in step keys and an "Extended step registry"
//! of keys reused across three or more chapters; both are always available
//! (global scope). Every other action key is chapter-local: a case may only use
//! it when its chapter's `NOTES.md` documents it. [`StepKind::scope`] encodes
//! that distinction; the loader enforces it against the parsed NOTES.

/// Whether a step action key is available in every chapter or only where its
/// chapter's `NOTES.md` documents it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepScope {
    /// A FORMAT.md built-in or Extended-step-registry key — always available.
    Global,
    /// A chapter-local key — available only where `NOTES.md` documents it.
    Chapter,
}

/// The action a step performs, identified by its leading member key.
///
/// Well-known keys map to named variants for ergonomic matching; anything else
/// falls to [`StepKind::Chapter`], the typed escape hatch for a chapter-local
/// step. The variant does not itself decide validity — [`StepKind::scope`]
/// does, together with the chapter's documented key set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepKind {
    // FORMAT.md built-ins (global).
    Connect,
    Disconnect,
    Call,
    Watch,
    Unwatch,
    ExpectView,
    AdvanceTime,
    Restart,
    Export,
    Import,
    Reconcile,
    BlobPut,
    BlobGet,
    Concurrently,
    // Extended step registry (global).
    HostLoad,
    ModuleInstall,
    TamperArtifact,
    ExpectClose,
    // Well-known chapter-local keys (chapter scope; still gated by NOTES).
    Operator,
    Reopen,
    KeyringAdmin,
    ProviderSet,
    ConnectorSet,
    RunReconciler,
    BudgetSet,
    ModuleUninstall,
    ModuleDisable,
    ModuleEnable,
    ModuleUpdate,
    ModuleRename,
    BuildArtifact,
    RepackArtifact,
    LoadArtifact,
    Restore,
    InSandbox,
    InspectArtifact,
    ExtractArtifact,
    ApplyCorrection,
    TamperExtract,
    ScrubScopeOfCascadedRow,
    Erase,
    Reinsert,
    Manifest,
    OperationStatus,
    Resume,
    Authenticate,
    /// An action key with no named variant, carried verbatim.
    Chapter(String),
}

impl StepKind {
    /// Classify an action key.
    #[must_use]
    pub fn from_key(key: &str) -> Self {
        match key {
            "connect" => Self::Connect,
            "disconnect" => Self::Disconnect,
            "call" => Self::Call,
            "watch" => Self::Watch,
            "unwatch" => Self::Unwatch,
            "expect_view" => Self::ExpectView,
            "advance_time" => Self::AdvanceTime,
            "restart" => Self::Restart,
            "export" => Self::Export,
            "import" => Self::Import,
            "reconcile" => Self::Reconcile,
            "blob_put" => Self::BlobPut,
            "blob_get" => Self::BlobGet,
            "concurrently" => Self::Concurrently,
            "host_load" => Self::HostLoad,
            "module_install" => Self::ModuleInstall,
            "tamper_artifact" => Self::TamperArtifact,
            "expect_close" => Self::ExpectClose,
            "operator" => Self::Operator,
            "reopen" => Self::Reopen,
            "keyring_admin" => Self::KeyringAdmin,
            "provider_set" => Self::ProviderSet,
            "connector_set" => Self::ConnectorSet,
            "run_reconciler" => Self::RunReconciler,
            "budget_set" => Self::BudgetSet,
            "module_uninstall" => Self::ModuleUninstall,
            "module_disable" => Self::ModuleDisable,
            "module_enable" => Self::ModuleEnable,
            "module_update" => Self::ModuleUpdate,
            "module_rename" => Self::ModuleRename,
            "build_artifact" => Self::BuildArtifact,
            "repack_artifact" => Self::RepackArtifact,
            "load_artifact" => Self::LoadArtifact,
            "restore" => Self::Restore,
            "in_sandbox" => Self::InSandbox,
            "inspect_artifact" => Self::InspectArtifact,
            "extract_artifact" => Self::ExtractArtifact,
            "apply_correction" => Self::ApplyCorrection,
            "tamper_extract" => Self::TamperExtract,
            "scrub_scope_of_cascaded_row" => Self::ScrubScopeOfCascadedRow,
            "erase" => Self::Erase,
            "reinsert" => Self::Reinsert,
            "manifest" => Self::Manifest,
            "operation_status" => Self::OperationStatus,
            "resume" => Self::Resume,
            "authenticate" => Self::Authenticate,
            other => Self::Chapter(other.to_owned()),
        }
    }

    /// The action key text.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Connect => "connect",
            Self::Disconnect => "disconnect",
            Self::Call => "call",
            Self::Watch => "watch",
            Self::Unwatch => "unwatch",
            Self::ExpectView => "expect_view",
            Self::AdvanceTime => "advance_time",
            Self::Restart => "restart",
            Self::Export => "export",
            Self::Import => "import",
            Self::Reconcile => "reconcile",
            Self::BlobPut => "blob_put",
            Self::BlobGet => "blob_get",
            Self::Concurrently => "concurrently",
            Self::HostLoad => "host_load",
            Self::ModuleInstall => "module_install",
            Self::TamperArtifact => "tamper_artifact",
            Self::ExpectClose => "expect_close",
            Self::Operator => "operator",
            Self::Reopen => "reopen",
            Self::KeyringAdmin => "keyring_admin",
            Self::ProviderSet => "provider_set",
            Self::ConnectorSet => "connector_set",
            Self::RunReconciler => "run_reconciler",
            Self::BudgetSet => "budget_set",
            Self::ModuleUninstall => "module_uninstall",
            Self::ModuleDisable => "module_disable",
            Self::ModuleEnable => "module_enable",
            Self::ModuleUpdate => "module_update",
            Self::ModuleRename => "module_rename",
            Self::BuildArtifact => "build_artifact",
            Self::RepackArtifact => "repack_artifact",
            Self::LoadArtifact => "load_artifact",
            Self::Restore => "restore",
            Self::InSandbox => "in_sandbox",
            Self::InspectArtifact => "inspect_artifact",
            Self::ExtractArtifact => "extract_artifact",
            Self::ApplyCorrection => "apply_correction",
            Self::TamperExtract => "tamper_extract",
            Self::ScrubScopeOfCascadedRow => "scrub_scope_of_cascaded_row",
            Self::Erase => "erase",
            Self::Reinsert => "reinsert",
            Self::Manifest => "manifest",
            Self::OperationStatus => "operation_status",
            Self::Resume => "resume",
            Self::Authenticate => "authenticate",
            Self::Chapter(key) => key,
        }
    }

    /// Whether this key is global (built-in / registry) or chapter-scoped.
    #[must_use]
    pub fn scope(&self) -> StepScope {
        match self {
            Self::Connect
            | Self::Disconnect
            | Self::Call
            | Self::Watch
            | Self::Unwatch
            | Self::ExpectView
            | Self::AdvanceTime
            | Self::Restart
            | Self::Export
            | Self::Import
            | Self::Reconcile
            | Self::BlobPut
            | Self::BlobGet
            | Self::Concurrently
            | Self::HostLoad
            | Self::ModuleInstall
            | Self::TamperArtifact
            | Self::ExpectClose => StepScope::Global,
            _ => StepScope::Chapter,
        }
    }
}
