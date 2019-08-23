use crate::build::BuildDirectory;
use crate::cmd::{Command, SandboxImage};
use crate::Toolchain;
use failure::{Error, ResultExt};
use log::info;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[cfg(windows)]
static DEFAULT_SANDBOX_IMAGE: &str = "rustops/crates-build-env-windows";

#[cfg(not(windows))]
static DEFAULT_SANDBOX_IMAGE: &str = "rustops/crates-build-env";

const DEFAULT_COMMAND_TIMEOUT: Option<Duration> = Some(Duration::from_secs(15 * 60));
const DEFAULT_COMMAND_NO_OUTPUT_TIMEOUT: Option<Duration> = None;

/// Builder of a [`Workspace`](struct.Workspace.html).
pub struct WorkspaceBuilder {
    user_agent: String,
    path: PathBuf,
    sandbox_image: Option<SandboxImage>,
    command_timeout: Option<Duration>,
    command_no_output_timeout: Option<Duration>,
    fast_init: bool,
}

impl WorkspaceBuilder {
    /// Create a new builder.
    ///
    /// The provided path will be the home of the workspace, containing all the data generated by
    /// rustwide (including state and caches).
    pub fn new(path: &Path, user_agent: &str) -> Self {
        Self {
            user_agent: user_agent.into(),
            path: path.into(),
            sandbox_image: None,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            command_no_output_timeout: DEFAULT_COMMAND_NO_OUTPUT_TIMEOUT,
            fast_init: false,
        }
    }

    /// Override the image used for sandboxes.
    ///
    /// By default rustwide will use the [rustops/crates-build-env] image on Linux systems, and
    /// [rustops/crates-build-env-windows] on Windows systems. Those images contain dependencies to
    /// build a large amount of crates.
    ///
    /// [rustops/crates-build-env]: https://hub.docker.com/rustops/crates-build-env
    /// [rustops/crates-build-env-windows]: https://hub.docker.com/rustops/crates-build-env-windows
    pub fn sandbox_image(mut self, image: SandboxImage) -> Self {
        self.sandbox_image = Some(image);
        self
    }

    /// Set the default timeout of [`Command`](cmd/struct.Command.html), which can be overridden
    /// with the [`Command::timeout`](cmd/struct.Command.html#method.timeout) method. To disable
    /// the timeout set its value to `None`. By default the timeout is 15 minutes.
    pub fn command_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Set the default no output timeout of [`Command`](cmd/struct.Command.html), which can be
    /// overridden with the
    /// [`Command::no_output_timeout`](cmd/struct.Command.html#method.no_output_timeout) method. To
    /// disable the timeout set its value to `None`. By default it's disabled.
    pub fn command_no_output_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.command_no_output_timeout = timeout;
        self
    }

    /// Enable or disable fast workspace initialization (disabled by default).
    ///
    /// Fast workspace initialization will change the initialization process to prefer
    /// initialization speed to runtime performance, for example by installing the tools rustwide
    /// needs in debug mode instead of release mode. It's not recommended to enable fast workspace
    /// initialization with production workloads, but it can help in CIs or other automated testing
    /// scenarios.
    pub fn fast_init(mut self, enable: bool) -> Self {
        self.fast_init = enable;
        self
    }

    /// Initialize the workspace. This will create all the necessary local files and fetch the rest from the network. It's
    /// not unexpected for this method to take minutes to run on slower network connections.
    pub fn init(self) -> Result<Workspace, Error> {
        std::fs::create_dir_all(&self.path).with_context(|_| {
            format!(
                "failed to create workspace directory: {}",
                self.path.display()
            )
        })?;

        crate::utils::file_lock(&self.path.join("lock"), "initialize the workspace", || {
            let sandbox_image = if let Some(img) = self.sandbox_image {
                img
            } else {
                SandboxImage::remote(DEFAULT_SANDBOX_IMAGE)?
            };

            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(reqwest::header::USER_AGENT, self.user_agent.parse()?);
            let http = reqwest::ClientBuilder::new()
                .default_headers(headers)
                .build()?;

            let ws = Workspace {
                inner: Arc::new(WorkspaceInner {
                    http,
                    path: self.path,
                    sandbox_image,
                    command_timeout: self.command_timeout,
                    command_no_output_timeout: self.command_no_output_timeout,
                }),
            };
            ws.init(self.fast_init)?;
            Ok(ws)
        })
    }
}

struct WorkspaceInner {
    http: reqwest::Client,
    path: PathBuf,
    sandbox_image: SandboxImage,
    command_timeout: Option<Duration>,
    command_no_output_timeout: Option<Duration>,
}

/// Directory on the filesystem containing rustwide's state and caches.
///
/// Use [`WorkspaceBuilder`](struct.WorkspaceBuilder.html) to create a new instance of it.
pub struct Workspace {
    inner: Arc<WorkspaceInner>,
}

impl Workspace {
    /// Open a named build directory inside the workspace.
    pub fn build_dir(&self, name: &str) -> BuildDirectory {
        BuildDirectory::new(
            Workspace {
                inner: self.inner.clone(),
            },
            name,
        )
    }

    /// Remove all the contents of all the build directories, freeing disk space.
    pub fn purge_all_build_dirs(&self) -> Result<(), Error> {
        std::fs::remove_dir_all(self.builds_dir())?;
        Ok(())
    }

    /// Return a list of all the toolchains present in the workspace.
    ///
    /// # Example
    ///
    /// This code snippet removes all the installed toolchains except the main one:
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// let main_toolchain = Toolchain::Dist { name: "stable".into() };
    /// for installed in &workspace.installed_toolchains()? {
    ///     if *installed != main_toolchain {
    ///         installed.uninstall(&workspace)?;
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn installed_toolchains(&self) -> Result<Vec<Toolchain>, Error> {
        crate::toolchain::list_installed(&self.rustup_home())
    }

    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) fn cargo_home(&self) -> PathBuf {
        self.inner.path.join("cargo-home")
    }

    pub(crate) fn rustup_home(&self) -> PathBuf {
        self.inner.path.join("rustup-home")
    }

    pub(crate) fn cache_dir(&self) -> PathBuf {
        self.inner.path.join("cache")
    }

    pub(crate) fn builds_dir(&self) -> PathBuf {
        self.inner.path.join("builds")
    }

    pub(crate) fn sandbox_image(&self) -> &SandboxImage {
        &self.inner.sandbox_image
    }

    pub(crate) fn default_command_timeout(&self) -> Option<Duration> {
        self.inner.command_timeout
    }

    pub(crate) fn default_command_no_output_timeout(&self) -> Option<Duration> {
        self.inner.command_no_output_timeout
    }

    fn init(&self, fast_init: bool) -> Result<(), Error> {
        info!("installing tools required by rustwide");
        crate::tools::install(self, fast_init)?;
        info!("updating the local crates.io registry clone");
        self.update_cratesio_registry()?;
        Ok(())
    }

    fn update_cratesio_registry(&self) -> Result<(), Error> {
        // This nop cargo command is to update the registry so we don't have to do it for each
        // crate.  using `install` is a temporary solution until
        // https://github.com/rust-lang/cargo/pull/5961 is ready

        let _ = Command::new(self, Toolchain::MAIN.cargo())
            .args(&["install", "lazy_static"])
            .no_output_timeout(None)
            .run();

        // ignore the error untill https://github.com/rust-lang/cargo/pull/5961 is ready
        Ok(())
    }
}
