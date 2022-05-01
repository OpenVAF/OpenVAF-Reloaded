xflags::xflags! {
    src "./src/flags.rs"

    /// Run custom build command.
    cmd xtask {
        default cmd help {
            /// Print help information.
            optional -h, --help
        }

        // cmd vendor{
        //     optional --force
        //     optional --no_upload
        //     optional --check
        // }

        // cmd cache {
        //     cmd prepare{}
        //     cmd create{}
        //     cmd upload{}
        //     cmd fetch{}
        //     cmd update{}
        // }

        cmd verilogae{
            cmd build{
                optional --force
                optional --manylinux
                optional --install
            }

            cmd test {
            }

            cmd publish {
            }

        }

    }
}
// generated start
// The following code is generated by `xflags` macro.
// Run `env UPDATE_XFLAGS=1 cargo build` to regenerate.
#[derive(Debug)]
pub struct Xtask {
    pub subcommand: XtaskCmd,
}

#[derive(Debug)]
pub enum XtaskCmd {
    Help(Help),
    Verilogae(Verilogae),
}

#[derive(Debug)]
pub struct Help {
    pub help: bool,
}

#[derive(Debug)]
pub struct Verilogae {
    pub subcommand: VerilogaeCmd,
}

#[derive(Debug)]
pub enum VerilogaeCmd {
    Build(Build),
    Test(Test),
    Publish(Publish),
}

#[derive(Debug)]
pub struct Build {
    pub force: bool,
    pub manylinux: bool,
    pub install: bool,
}

#[derive(Debug)]
pub struct Test;

#[derive(Debug)]
pub struct Publish;

impl Xtask {
    pub const HELP: &'static str = Self::HELP_;

    #[allow(dead_code)]
    pub fn from_env() -> xflags::Result<Self> {
        Self::from_env_()
    }

    #[allow(dead_code)]
    pub fn from_vec(args: Vec<std::ffi::OsString>) -> xflags::Result<Self> {
        Self::from_vec_(args)
    }
}
// generated end
