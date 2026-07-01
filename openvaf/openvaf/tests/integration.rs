use std::f64::consts;
use std::ffi::CStr;
use std::ffi::OsStr;
use std::path::Path;

use camino::Utf8Path;
use expect_test::expect_file;
use float_cmp::assert_approx_eq;
use mini_harness::{harness, Result};
use openvaf::{CompilationDestination, CompilationTermination, LLVMCodeGenOptLevel};
use stdx::{ignore_dev_tests, openvaf_test_data, project_root};
use target::spec::Target;

use crate::load::{load_osdi_lib, EvalFlags, OsdiDescriptor, OsdiInstance, OsdiModel};
use crate::mock_sim::{MockSimulation, ALPHA};

mod load;
mod mock_sim;

fn compile_and_load(root_file: &Utf8Path) -> &'static OsdiDescriptor {
    let libs = compile_and_load_all(root_file);
    assert_eq!(libs.len(), 1);
    &libs[0]
}

fn compile_and_load_all(root_file: &Utf8Path) -> &'static [OsdiDescriptor] {
    let openvaf_opts = openvaf::Opts {
        defines: Vec::new(),
        codegen_opts: Vec::new(),
        lints: Vec::new(),
        input: root_file.to_path_buf(),
        output: CompilationDestination::Path { lib_file: root_file.with_extension("osdi") },
        include: Vec::new(),
        opt_lvl: LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive,
        target: Target::host_target().expect(
            "Failed to determine host target. This architecture may not be supported by OpenVAF. \
             Supported targets include: x86_64-unknown-linux, aarch64-unknown-linux, riscv64-unknown-linux, etc."
        ),
        target_cpu: "native".to_owned(),
        dry_run: false,
        dump_mir: false,
        dump_unopt_mir: false,
        dump_ir: false,
        dump_unopt_ir: false,
    };

    let res = openvaf::compile(&openvaf_opts).unwrap();
    let lib_file = match res {
        CompilationTermination::Compiled { lib_file } => lib_file,
        CompilationTermination::FatalDiagnostic => {
            panic!("openvaf: compilation of {root_file} failed");
        }
    };
    unsafe { load_osdi_lib(&lib_file).unwrap() }
}

// fn integration_test(dir: &str) -> Result {
//     let path: Utf8PathBuf = project_root().join("integration_tests").try_into().unwrap();
//     let name = dir.to_lowercase();
//     let main_file = path.join(dir).join(format!("{name}.va"));
//     let device = compile_and_load(&main_file);

//     Ok(())
// }

fn integration_test(dir: &Path) -> Result {
    let name = dir.file_name().unwrap().to_str().unwrap().to_lowercase();
    let main_file = dir.join(format!("{name}.va"));
    test_descriptor(&main_file)?;
    Ok(())
}

/// Test a single Verilog-A file directly (for VACASK models)
/// Uses "vacask_" prefix for snapshot names to avoid conflicts with OpenVAF models
fn vacask_test(file: &Path) -> Result {
    test_descriptor_with_prefix(file, "vacask_")?;
    Ok(())
}

/// Test a single Verilog-A file with SPICE naming prefix
fn vacask_spice_test(file: &Path) -> Result {
    test_descriptor_with_prefix(file, "vacask_spice_")?;
    Ok(())
}

/// Test a single Verilog-A file with simplified SPICE naming prefix
fn vacask_spice_sn_test(file: &Path) -> Result {
    test_descriptor_with_prefix(file, "vacask_spice_sn_")?;
    Ok(())
}

/// Filter to only include .va files
fn is_va_file(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("va"))
}

/// Get path to VACASK devices directory
fn vacask_devices() -> std::path::PathBuf {
    project_root().join("external/vacask/devices")
}

fn test_descriptor(main_file: &Path) -> Result<&'static OsdiDescriptor> {
    test_descriptor_with_prefix(main_file, "")
}

fn test_descriptor_with_prefix(main_file: &Path, prefix: &str) -> Result<&'static OsdiDescriptor> {
    let main_file: &Utf8Path = main_file.try_into().unwrap();
    let name = main_file.file_stem().unwrap();
    let desc = compile_and_load(main_file);
    let expect = format!("{desc:?}");
    let test_dir = openvaf_test_data("osdi");
    expect_file![test_dir.join(format!("{prefix}{name}.snap"))].assert_eq(&expect);
    let default_model = desc.new_model();
    default_model.process_params()?;
    let mut instance = default_model.new_instance();
    instance.process_params(&default_model, desc.num_terminals, 300.0)?;
    Ok(desc)
}

macro_rules! assert_approx_eq {
    ($val: expr, $resist: expr, $react: expr) => {
        let (resist, react) = $val;
        let resist_ref: f64 = $resist;
        if (resist - resist_ref).abs() / resist.min(resist_ref) >= 0.01 {
            float_cmp::assert_approx_eq!(f64, resist, resist_ref, epsilon = 1e-10)
        }
        let react_ref: f64 = $react;
        if (react - react_ref).abs() / react.min(react_ref) >= 0.01 {
            float_cmp::assert_approx_eq!(f64, react, react_ref, epsilon = 1e-10)
        }
    };
}

fn test_limit() -> Result<()> {
    // skipping in CI for now as we don't have a toolchain there
    // currently
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    const KB: f64 = 1.3806488e-23;
    const Q: f64 = 1.602176565e-19;
    const VT: f64 = KB * 300.0 / Q;
    const IS: f64 = 1e-12;
    const CJ0: f64 = 10e-9;
    let vcrit = VT * f64::ln(VT / (consts::SQRT_2 * IS));
    let check_dae_equations = |sim: &MockSimulation, vd_lim, vd| {
        let id = |vd| IS * (f64::exp(vd / VT) - 1.0);
        let id_vd = |vd| IS / VT * f64::exp(vd / VT);
        let cj = |vd| CJ0 * vd;
        assert_approx_eq!(sim.read_jacobian("A", "A"), id_vd(vd_lim), CJ0);
        assert_approx_eq!(sim.read_jacobian("C", "C"), id_vd(vd_lim), CJ0);
        assert_approx_eq!(sim.read_jacobian("A", "C"), -id_vd(vd_lim), -CJ0);
        assert_approx_eq!(sim.read_jacobian("C", "A"), -id_vd(vd_lim), -CJ0);
        assert_approx_eq!(
            sim.read_residual("A"),
            id(vd_lim) - id_vd(vd_lim) * (vd_lim - vd),
            cj(vd_lim) - CJ0 * (vd_lim - vd)
        );
        assert_approx_eq!(
            sim.read_residual("C"),
            id_vd(vd_lim) * (vd_lim - vd) - id(vd_lim),
            CJ0 * (vd_lim - vd) - cj(vd_lim)
        );
    };

    let check_spice_equations = |sim: &MockSimulation, vd_lim, vd| {
        let id = |vd| IS * (f64::exp(vd / VT) - 1.0);
        let id_vd = |vd| IS / VT * f64::exp(vd / VT);
        let cj = |vd| CJ0 * vd;
        assert_approx_eq!(
            sim.read_residual("A"),
            id_vd(vd_lim) * vd_lim - id(vd_lim) + ALPHA * (CJ0 * vd_lim - cj(vd_lim)),
            0.0
        );
        assert_approx_eq!(
            sim.read_residual("C"),
            id_vd(vd_lim) * (vd_lim - vd) - id(vd_lim),
            CJ0 * (vd_lim - vd) - cj(vd_lim)
        );
        assert_approx_eq!(sim.read_jacobian("A", "A"), id_vd(vd_lim) + ALPHA * CJ0, 0.0);
        assert_approx_eq!(sim.read_jacobian("C", "C"), id_vd(vd_lim) + ALPHA * CJ0, 0.0);
        assert_approx_eq!(sim.read_jacobian("A", "C"), -id_vd(vd_lim) - ALPHA * CJ0, 0.0);
        assert_approx_eq!(sim.read_jacobian("C", "A"), -id_vd(vd_lim) - ALPHA * CJ0, 0.0);
    };

    // compile model and setup simulation
    let desc = test_descriptor(&openvaf_test_data("osdi").join("diode_lim.va"))?;
    let model = desc.new_model();
    model.set_real_param(1, IS);
    model.set_real_param(5, CJ0);
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    instance.eval(&model, &mut sim, EvalFlags::INIT_LIM | EvalFlags::ENABLE_LIM);
    instance.load_dae(&model, &mut sim);
    check_dae_equations(&sim, vcrit, 0.0);
    sim.clear();
    instance.load_spice(&model, &mut sim);
    check_spice_equations(&sim, vcrit, 0.0);

    sim.next_iter();
    sim.set_voltage("A", 2.0 * vcrit);
    instance.eval(&model, &mut sim, EvalFlags::ENABLE_LIM);
    instance.load_dae(&model, &mut sim);
    check_dae_equations(&sim, 1.5 * vcrit, 2.0 * vcrit);
    sim.clear();
    instance.load_spice(&model, &mut sim);
    check_spice_equations(&sim, 1.5 * vcrit, 2.0 * vcrit);
    Ok(())
}

macro_rules! assert_approx_eq {
    ($val: expr, $expect: expr) => {
        let resist = $val;
        let resist_ref: f64 = $expect;
        if (resist - resist_ref).abs() / resist.min(resist_ref) >= 0.01 {
            float_cmp::assert_approx_eq!(f64, resist, resist_ref, epsilon = 1e-10)
        }
    };
}

fn test_noise() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    // skipping in CI for now as we don't have a toolchain there
    // currently
    const MFACTOR: f64 = 2.0;
    const PWR: f64 = 3.0;
    const EXP: f64 = 7.0;
    const V_AC: f64 = 13.0;

    // compile model and setup simulation
    let desc = test_descriptor(&openvaf_test_data("osdi").join("noise.va"))?;
    let model = desc.new_model();
    model.set_real_param(0, MFACTOR);
    model.set_real_param(1, PWR);
    model.set_real_param(2, EXP);
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    sim.set_voltage("a", V_AC);
    instance.eval(&model, &mut sim, EvalFlags::empty());
    for freq in 1..10 {
        let freq = freq as f64;
        instance.load_noise(&model, &mut sim, freq);
        let white_noise1 = MFACTOR * PWR * V_AC;
        let white_noise2 = MFACTOR * PWR * PWR * V_AC;
        let flickr_noise1 = MFACTOR * V_AC * PWR * PWR / (freq.powf(EXP));
        let flickr_noise2 = MFACTOR * PWR * PWR / (freq.powf(EXP * V_AC));
        assert_approx_eq!(sim.read_noise(0), white_noise1);
        assert_approx_eq!(sim.read_noise(1), white_noise2);
        assert_approx_eq!(sim.read_noise(2), flickr_noise1);
        assert_approx_eq!(sim.read_noise(3), flickr_noise2);
    }
    Ok(())
}

/// Fixed-size arrays: declaration, constant- and runtime-index read/write all
/// feed a single conductance. See `arrays.va`; with the default `sel=1` the
/// assembled conductance is G = 21, so the loaded DAE residual/Jacobian must
/// match exactly if every array access lowered correctly.
fn test_arrays() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let desc = test_descriptor(&openvaf_test_data("osdi").join("arrays.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    sim.set_voltage("p", 1.0);
    sim.set_voltage("n", 0.0);
    instance.eval(&model, &mut sim, EvalFlags::empty());
    instance.load_dae(&model, &mut sim);

    // G = gsum (13) + gx (8) = 21, with I(p,n) = G * V(p,n).
    float_cmp::assert_approx_eq!(f64, sim.read_residual("p").0, 21.0, epsilon = 1e-9);
    float_cmp::assert_approx_eq!(f64, sim.read_residual("n").0, -21.0, epsilon = 1e-9);
    float_cmp::assert_approx_eq!(f64, sim.read_jacobian("p", "p").0, 21.0, epsilon = 1e-9);
    Ok(())
}

/// `@(cross)` state retention: `state` is assigned only inside cross handlers, so
/// it must hold across timesteps. The mock simulator's `next_iter` swaps the
/// prev/next state arrays, exactly as a real simulator advances a timestep. We
/// drive the input high/low/dead-band and check the latched output is retained.
/// See `cross_latch.va`; residual at q equals `-state` (with V(q)=0).
fn test_cross_latch() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let desc = test_descriptor(&openvaf_test_data("osdi").join("cross_latch.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    // Advance one timestep: swap prev/next state, re-apply the node voltages
    // (next_iter zeroes the solution), evaluate, and load the DAE residual.
    let step = |instance: &OsdiInstance,
                model: &OsdiModel,
                sim: &mut MockSimulation,
                vd: f64,
                first: bool| {
        if !first {
            sim.next_iter();
        }
        sim.set_voltage("q", 0.0);
        sim.set_voltage("d", vd);
        instance.eval(model, sim, EvalFlags::ENABLE_LIM | EvalFlags::INIT_LIM);
        instance.load_dae(model, sim);
        sim.read_residual("q").0
    };

    // d high -> latch sets state=1 (residual = -1).
    float_cmp::assert_approx_eq!(
        f64,
        step(&instance, &model, &mut sim, 1.0, true),
        -1.0,
        epsilon = 1e-9
    );
    // dead-band -> state 1 retained.
    float_cmp::assert_approx_eq!(
        f64,
        step(&instance, &model, &mut sim, 0.5, false),
        -1.0,
        epsilon = 1e-9
    );
    // d low -> latch clears state=0 (residual = 0).
    float_cmp::assert_approx_eq!(
        f64,
        step(&instance, &model, &mut sim, 0.0, false),
        0.0,
        epsilon = 1e-9
    );
    // dead-band -> state 0 retained.
    float_cmp::assert_approx_eq!(
        f64,
        step(&instance, &model, &mut sim, 0.5, false),
        0.0,
        epsilon = 1e-9
    );
    // d high again -> latch flips back to state=1.
    float_cmp::assert_approx_eq!(
        f64,
        step(&instance, &model, &mut sim, 1.0, false),
        -1.0,
        epsilon = 1e-9
    );
    Ok(())
}

/// Regression: `laplace_nd` with anonymous integer coefficient literals (the form
/// the LRM examples use) must compile without the optimizer panicking on mixed
/// int/float arithmetic. Compiling + loading the descriptor is enough to guard the
/// crash. See `laplace_nd_int.va`.
fn test_laplace_nd_int() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }
    test_descriptor(&openvaf_test_data("osdi").join("laplace_nd_int.va"))?;
    Ok(())
}

/// Regression: module instances with vector formal ports and part-select actuals
/// are flattened into the parent analog body. The duplicate `tmp` discipline
/// declaration is intentionally same-discipline and must not be rejected.
fn test_module_inst_part_select() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }
    let descs = compile_and_load_all(
        openvaf_test_data("osdi").join("module_inst_part_select.va").as_path().try_into().unwrap(),
    );
    let desc = descs
        .iter()
        .find(|desc| unsafe { CStr::from_ptr(desc.name).to_str().unwrap() }
            == "module_inst_part_select")
        .expect("parent module descriptor missing");
    assert_eq!(desc.num_terminals, 8);
    Ok(())
}

/// Regression: `$rdist_normal(seed, mean, sigma)` lowers into generated code and
/// does not require an OSDI simulator callback or ABI extension.
fn test_rdist_normal() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }
    compile_and_load(
        openvaf_test_data("osdi").join("rdist_normal.va").as_path().try_into().unwrap(),
    );
    Ok(())
}

/// Regression: zero-start periodic `@(timer(0, period))` lowers without a VACASK
/// ABI extension, and `$rdist_normal(seed, ...)` side-effect state is retained
/// when sampled inside the timer body.
fn test_rdist_timer() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }
    compile_and_load(
        openvaf_test_data("osdi").join("rdist_timer.va").as_path().try_into().unwrap(),
    );
    Ok(())
}

/// Vectored/bus ports: a port declared bare in the head and ranged in the body
/// (`input [0:3] in`) must expand to in[0]..in[3] and index correctly. The output
/// sums the four bits with distinct weights, so the loaded residual proves each bit
/// is a distinct node. See `vector_ports.va`.
fn test_vector_ports() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }
    let desc = test_descriptor(&openvaf_test_data("osdi").join("vector_ports.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    sim.set_voltage("out", 0.0);
    sim.set_voltage("in[0]", 1.0);
    sim.set_voltage("in[1]", 1.0);
    sim.set_voltage("in[2]", 1.0);
    sim.set_voltage("in[3]", 1.0);
    instance.eval(&model, &mut sim, EvalFlags::empty());
    instance.load_dae(&model, &mut sim);

    // residual(out) = V(out) - (1+2+3+4) = -10.
    float_cmp::assert_approx_eq!(f64, sim.read_residual("out").0, -10.0, epsilon = 1e-9);
    Ok(())
}

/// LRM 2.4 transition() Example 1 (QAM modulator): vectored input ports declared
/// bare in the head and ranged in the body, bus indexing, transition, $abstime.
/// Compile+load guard.
fn test_qam16() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }
    test_descriptor(&openvaf_test_data("osdi").join("qam16.va"))?;
    Ok(())
}

/// Retained `@(cross)` ARRAY state: each array element assigned inside a cross
/// handler must retain independently across timesteps. Drives the input
/// high/dead-band/low and checks both elements hold and flip via the prev/next
/// state swap. See `cross_array.va`; residual at q0/q1 equals -s[0]/-s[1].
fn test_cross_array() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let desc = test_descriptor(&openvaf_test_data("osdi").join("cross_array.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    let step = |instance: &OsdiInstance,
                model: &OsdiModel,
                sim: &mut MockSimulation,
                vd: f64,
                first: bool| {
        if !first {
            sim.next_iter();
        }
        sim.set_voltage("q0", 0.0);
        sim.set_voltage("q1", 0.0);
        sim.set_voltage("d", vd);
        instance.eval(model, sim, EvalFlags::ENABLE_LIM | EvalFlags::INIT_LIM);
        instance.load_dae(model, sim);
        (sim.read_residual("q0").0, sim.read_residual("q1").0)
    };

    let check = |(a, b): (f64, f64), ea: f64, eb: f64| {
        float_cmp::assert_approx_eq!(f64, a, ea, epsilon = 1e-9);
        float_cmp::assert_approx_eq!(f64, b, eb, epsilon = 1e-9);
    };

    check(step(&instance, &model, &mut sim, 1.0, true), -1.0, -2.0); // set s=[1,2]
    check(step(&instance, &model, &mut sim, 0.5, false), -1.0, -2.0); // dead-band: retained
    check(step(&instance, &model, &mut sim, 0.0, false), 0.0, 0.0); // clear s=[0,0]
    check(step(&instance, &model, &mut sim, 0.5, false), 0.0, 0.0); // dead-band: retained
    check(step(&instance, &model, &mut sim, 1.0, false), -1.0, -2.0); // flips back
    Ok(())
}

/// Indirect branch assignment `V(out) : V(pin,nin) == 0` (ideal op-amp, issue #80).
/// Lowers to an implicit equation whose unknown drives `out` as a voltage source and
/// whose residual is the constraint `V(pin) - V(nin)`. The constraint residual (and
/// its Jacobian) is independent of the unknown, so we can check it on the isolated
/// device: with V(pin)=0.3, V(nin)=0.1 the `implicit_equation_0` row carries 0.2 with
/// d/dV(pin)=+1, d/dV(nin)=-1. See `opamp_indirect.va`.
fn test_indirect_opamp() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let desc = test_descriptor(&openvaf_test_data("osdi").join("opamp_indirect.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    sim.set_voltage("out", 0.0);
    sim.set_voltage("pin", 0.3);
    sim.set_voltage("nin", 0.1);
    sim.set_voltage("implicit_equation_0", 0.7); // unknown; residual must not depend on it
    instance.eval(&model, &mut sim, EvalFlags::empty());
    instance.load_dae(&model, &mut sim);

    // constraint residual = V(pin) - V(nin) = 0.2, regardless of the unknown.
    float_cmp::assert_approx_eq!(
        f64,
        sim.read_residual("implicit_equation_0").0,
        0.2,
        epsilon = 1e-9
    );
    float_cmp::assert_approx_eq!(
        f64,
        sim.read_jacobian("pin", "implicit_equation_0").0,
        1.0,
        epsilon = 1e-9
    );
    float_cmp::assert_approx_eq!(
        f64,
        sim.read_jacobian("nin", "implicit_equation_0").0,
        -1.0,
        epsilon = 1e-9
    );
    Ok(())
}

/// LRM 2.4 transition() Example 2 (N-bit A/D converter), legal form (continuous
/// contributions outside the discrete @(cross) sampler). Exercises the whole new
/// stack at once: vector ports + genvar unroll + retained @(cross) array +
/// transition. Compile+load guard.
fn test_adc() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }
    test_descriptor(&openvaf_test_data("osdi").join("adc.va"))?;
    Ok(())
}

/// Regression: a statically bounded ordinary integer loop may index vectored
/// nodes in branch contributions. This is statically elaborated before HIR name
/// resolution so expressions such as `V(vout[i]) <+ V(vin[i+1])` resolve to the
/// concrete scalar terminals.
fn test_runtime_bus_index() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }
    compile_and_load(
        openvaf_test_data("osdi").join("runtime_bus_index.va").as_path().try_into().unwrap(),
    );
    Ok(())
}

harness! {
    // TODO: run this in CI, somehow this test is flakey tough regarding the linker invocation (and really slow)
    Test::from_dir("integration", &integration_test, &ignore_dev_tests, &project_root().join("integration_tests")),
    // VACASK basic device models
    Test::from_dir_filtered("vacask", &vacask_test, &is_va_file, &ignore_dev_tests, &vacask_devices()),
    // VACASK SPICE models
    Test::from_dir_filtered("vacask_spice", &vacask_spice_test, &is_va_file, &ignore_dev_tests, &vacask_devices().join("spice")),
    // VACASK simplified SPICE models
    Test::from_dir_filtered("vacask_spice_sn", &vacask_spice_sn_test, &is_va_file, &ignore_dev_tests, &vacask_devices().join("spice/sn")),
    [Test::new("$limit", &test_limit),Test::new("noise", &test_noise),Test::new("arrays", &test_arrays),Test::new("cross_latch", &test_cross_latch),Test::new("laplace_nd_int", &test_laplace_nd_int),Test::new("module_inst_part_select", &test_module_inst_part_select),Test::new("rdist_normal", &test_rdist_normal),Test::new("rdist_timer", &test_rdist_timer),Test::new("vector_ports", &test_vector_ports),Test::new("qam16", &test_qam16),Test::new("cross_array", &test_cross_array),Test::new("adc", &test_adc),Test::new("runtime_bus_index", &test_runtime_bus_index),Test::new("indirect_opamp", &test_indirect_opamp)]
}
