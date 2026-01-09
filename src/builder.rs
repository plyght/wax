use crate::error::{Result, WaxError};
use crate::formula_parser::{BuildSystem, ParsedFormula};
use indicatif::ProgressBar;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, instrument};

pub struct Builder {
    num_cores: usize,
    use_ccache: bool,
}

impl Builder {
    pub fn new() -> Self {
        let num_cores = Self::detect_cpu_cores();
        let use_ccache = Self::detect_ccache();

        info!(
            "Builder initialized: {} cores, ccache: {}",
            num_cores, use_ccache
        );

        Self {
            num_cores,
            use_ccache,
        }
    }

    fn detect_cpu_cores() -> usize {
        num_cpus::get().saturating_sub(1).max(1)
    }

    fn detect_ccache() -> bool {
        Command::new("which")
            .arg("ccache")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    #[instrument(skip(self, progress))]
    pub async fn build_from_source(
        &self,
        formula: &ParsedFormula,
        source_tarball: &Path,
        build_dir: &Path,
        install_prefix: &Path,
        progress: Option<&ProgressBar>,
    ) -> Result<()> {
        info!("Building {} from source", formula.name);

        if let Some(pb) = progress {
            pb.set_message("Extracting source...");
        }

        self.extract_source(source_tarball, build_dir).await?;

        let source_dir = self.find_source_directory(build_dir)?;

        if let Some(pb) = progress {
            pb.set_message("Configuring build...");
        }

        match formula.build_system {
            BuildSystem::Autotools => {
                self.build_autotools(&source_dir, install_prefix, &formula.configure_args)
                    .await?
            }
            BuildSystem::CMake => {
                self.build_cmake(&source_dir, install_prefix, &formula.configure_args)
                    .await?
            }
            BuildSystem::Meson => {
                self.build_meson(&source_dir, install_prefix, &formula.configure_args)
                    .await?
            }
            BuildSystem::Make => self.build_make(&source_dir, install_prefix).await?,
            BuildSystem::Unknown => {
                return Err(WaxError::BuildError(
                    "Unknown build system - cannot build from source".to_string(),
                ))
            }
        }

        if let Some(pb) = progress {
            pb.set_message("Build complete");
        }

        Ok(())
    }

    async fn extract_source(&self, tarball: &Path, dest: &Path) -> Result<()> {
        debug!("Extracting {:?} to {:?}", tarball, dest);

        tokio::fs::create_dir_all(dest).await?;

        let output = Command::new("tar")
            .arg("xzf")
            .arg(tarball)
            .arg("-C")
            .arg(dest)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(WaxError::BuildError(format!(
                "Failed to extract source: {}",
                stderr
            )));
        }

        Ok(())
    }

    fn find_source_directory(&self, build_dir: &Path) -> Result<PathBuf> {
        let entries = std::fs::read_dir(build_dir)?
            .filter_map(|e| e.ok())
            .collect::<Vec<_>>();

        if entries.len() == 1 {
            let entry = &entries[0];
            if entry.file_type()?.is_dir() {
                return Ok(entry.path());
            }
        }

        Ok(build_dir.to_path_buf())
    }

    async fn build_autotools(
        &self,
        source_dir: &Path,
        prefix: &Path,
        configure_args: &[String],
    ) -> Result<()> {
        info!("Building with autotools");

        let configure_script = source_dir.join("configure");
        if !configure_script.exists() {
            let bootstrap = source_dir.join("bootstrap");
            if bootstrap.exists() {
                self.run_command(source_dir, "./bootstrap", &[], "Bootstrapping")?;
            } else {
                let autogen = source_dir.join("autogen.sh");
                if autogen.exists() {
                    self.run_command(source_dir, "./autogen.sh", &[], "Generating build files")?;
                }
            }
        }

        let mut args = vec![format!("--prefix={}", prefix.display())];
        args.extend(configure_args.iter().cloned());

        self.run_command(source_dir, "./configure", &args, "Configuring")?;

        let make_args = vec![format!("-j{}", self.num_cores)];
        self.run_command(source_dir, "make", &make_args, "Compiling")?;

        self.run_command(source_dir, "make", &["install".to_string()], "Installing")?;

        Ok(())
    }

    async fn build_cmake(
        &self,
        source_dir: &Path,
        prefix: &Path,
        configure_args: &[String],
    ) -> Result<()> {
        info!("Building with CMake");

        let build_dir = source_dir.join("build");
        tokio::fs::create_dir_all(&build_dir).await?;

        let prefer_ninja = Self::has_ninja();
        let generator = if prefer_ninja {
            "Ninja"
        } else {
            "Unix Makefiles"
        };

        let mut args = vec![
            "-S".to_string(),
            source_dir.display().to_string(),
            "-B".to_string(),
            build_dir.display().to_string(),
            format!("-DCMAKE_INSTALL_PREFIX={}", prefix.display()),
            format!("-G{}", generator),
        ];
        args.extend(configure_args.iter().cloned());

        self.run_command(source_dir, "cmake", &args, "Configuring CMake")?;

        let build_args = vec![
            "--build".to_string(),
            build_dir.display().to_string(),
            "--parallel".to_string(),
            self.num_cores.to_string(),
        ];
        self.run_command(source_dir, "cmake", &build_args, "Building")?;

        let install_args = vec!["--install".to_string(), build_dir.display().to_string()];
        self.run_command(source_dir, "cmake", &install_args, "Installing")?;

        Ok(())
    }

    async fn build_meson(
        &self,
        source_dir: &Path,
        prefix: &Path,
        configure_args: &[String],
    ) -> Result<()> {
        info!("Building with Meson");

        let build_dir = source_dir.join("build");

        let mut args = vec![
            "setup".to_string(),
            build_dir.display().to_string(),
            format!("--prefix={}", prefix.display()),
        ];
        args.extend(configure_args.iter().cloned());

        self.run_command(source_dir, "meson", &args, "Configuring Meson")?;

        let ninja_args = vec![
            "-C".to_string(),
            build_dir.display().to_string(),
            format!("-j{}", self.num_cores),
        ];
        self.run_command(source_dir, "ninja", &ninja_args, "Building")?;

        let install_args = vec![
            "-C".to_string(),
            build_dir.display().to_string(),
            "install".to_string(),
        ];
        self.run_command(source_dir, "ninja", &install_args, "Installing")?;

        Ok(())
    }

    async fn build_make(&self, source_dir: &Path, prefix: &Path) -> Result<()> {
        info!("Building with Make");

        let make_args = vec![
            format!("PREFIX={}", prefix.display()),
            format!("-j{}", self.num_cores),
        ];
        self.run_command(source_dir, "make", &make_args, "Building")?;

        let install_args = vec![
            format!("PREFIX={}", prefix.display()),
            "install".to_string(),
        ];
        self.run_command(source_dir, "make", &install_args, "Installing")?;

        Ok(())
    }

    fn run_command(
        &self,
        work_dir: &Path,
        program: &str,
        args: &[String],
        phase: &str,
    ) -> Result<()> {
        debug!("{}: {} {:?}", phase, program, args);

        let mut cmd = Command::new(program);
        cmd.current_dir(work_dir);

        for arg in args {
            cmd.arg(arg);
        }

        if self.use_ccache && (program == "gcc" || program == "clang" || program == "cc") {
            cmd.env("CC", "ccache gcc");
            cmd.env("CXX", "ccache g++");
        }

        cmd.env("MAKEFLAGS", format!("-j{}", self.num_cores));

        let output = cmd.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let last_lines: Vec<&str> = stderr.lines().rev().take(50).collect();
            return Err(WaxError::BuildError(format!(
                "{} failed:\n{}",
                phase,
                last_lines.into_iter().rev().collect::<Vec<_>>().join("\n")
            )));
        }

        Ok(())
    }

    fn has_ninja() -> bool {
        Command::new("which")
            .arg("ninja")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}
