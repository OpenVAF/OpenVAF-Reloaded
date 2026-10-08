use std::f64::consts;
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
    let libs = unsafe { load_osdi_lib(&lib_file).unwrap() };
    assert_eq!(libs.len(), 1);
    &libs[0]
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

    // Advance one timestep: swap prev/next state, move time on (a monitored event
    // only fires once the simulation has advanced from zero), re-apply the node
    // voltages (next_iter zeroes the solution), evaluate, load the DAE residual.
    let mut step = |instance: &OsdiInstance,
                    model: &OsdiModel,
                    sim: &mut MockSimulation,
                    vd: f64,
                    first: bool| {
        if !first {
            sim.next_iter();
        }
        sim.advance_time(1e-6);
        sim.set_voltage("q", 0.0);
        sim.set_voltage("d", vd);
        instance.eval(model, sim, EvalFlags::ENABLE_LIM | EvalFlags::INIT_LIM);
        instance.load_dae(model, sim);
        sim.read_residual("q").0
    };

    // Now that `cross` takes part in scheduling, the stimulus has to actually
    // cross: starting at d high would step from the initial state straight past the
    // threshold with nothing to cross *from*. Settle low first, state still 0.
    float_cmp::assert_approx_eq!(
        f64,
        step(&instance, &model, &mut sim, 0.0, true),
        0.0,
        epsilon = 1e-9
    );
    // d crosses 0.7 upward -> latch sets state=1 (residual = -1).
    float_cmp::assert_approx_eq!(
        f64,
        step(&instance, &model, &mut sim, 1.0, false),
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

    let mut step = |instance: &OsdiInstance,
                    model: &OsdiModel,
                    sim: &mut MockSimulation,
                    vd: f64,
                    first: bool| {
        if !first {
            sim.next_iter();
        }
        sim.advance_time(1e-6);
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

    // settle low first: a monitored event needs something to cross *from*
    check(step(&instance, &model, &mut sim, 0.0, true), 0.0, 0.0);
    check(step(&instance, &model, &mut sim, 1.0, false), -1.0, -2.0); // set s=[1,2]
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

/// VAMS-2023 4.5.11: a null zeros argument (`laplace_nd(V(in), , den)`) means the
/// empty product of zeros, so the numerator is unity and the filter is `1/D(s)`.
/// The null has no expression behind it, so this also guards that it never reaches
/// the expression lowering, which panics on a missing expression. See
/// `laplace_null_zeros.va`.
fn test_laplace_null_zeros() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    const TAU: f64 = 1e-6;

    let desc = test_descriptor(&openvaf_test_data("osdi").join("laplace_null_zeros.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    // One pole, so one state. `1 + s*tau` over a unity numerator lowers to
    // `dx/dt = (input - x)/tau` with the output taken straight from the state:
    // resist = -(input - x)/tau, react = x.
    sim.set_voltage("in", 1.0);
    sim.set_voltage("out", 0.0);
    sim.set_voltage("implicit_equation_0", 0.25);
    instance.eval(&model, &mut sim, EvalFlags::empty());
    instance.load_dae(&model, &mut sim);

    let (resist, react) = sim.read_residual("implicit_equation_0");
    float_cmp::assert_approx_eq!(f64, resist, -(1.0 - 0.25) / TAU, epsilon = 1e-3);
    float_cmp::assert_approx_eq!(f64, react, 0.25, epsilon = 1e-9);

    // A unity numerator, not a zero one: the output follows the state, so the
    // contribution is not identically zero.
    float_cmp::assert_approx_eq!(f64, sim.read_residual("flow(out)").0, 0.25, epsilon = 1e-9);

    Ok(())
}

/// VAMS-2023 4.5.11.1-4.5.11.3: the root forms of the Laplace filter. Each one is
/// paired in the model with a `laplace_nd` call whose coefficients spell out the
/// same transfer function, so expanding the roots must reproduce them exactly --
/// including a zero *at* zero, whose factor is a bare `s` rather than `1 - s/r`,
/// and a conjugate pole pair, whose imaginary parts have to cancel. See
/// `laplace_roots.va`.
fn test_laplace_roots() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let desc = test_descriptor(&openvaf_test_data("osdi").join("laplace_roots.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    sim.set_voltage("in", 1.0);
    for out in ["o1", "o2", "o3", "o4", "o5", "o6", "o7", "o8"] {
        sim.set_voltage(out, 0.0);
    }
    // (root form, coefficient form) equation pairs, in source order: one state each
    // for the two first-order filters, then two each for the second-order pair.
    const PAIRS: [(usize, usize); 6] = [(0, 1), (2, 3), (4, 6), (5, 7), (8, 10), (9, 11)];

    // The two equations of a pair have to hold the same state to be comparable, but
    // the values differ *between* pairs so that a mix-up between the two states of
    // the second-order filter cannot pass by coincidence.
    for (i, (root_eq, coeff_eq)) in PAIRS.iter().enumerate() {
        let x = 0.25 + 0.125 * i as f64;
        sim.set_voltage(&format!("implicit_equation_{root_eq}"), x);
        sim.set_voltage(&format!("implicit_equation_{coeff_eq}"), x);
    }

    instance.eval(&model, &mut sim, EvalFlags::empty());
    instance.load_dae(&model, &mut sim);

    for (root_eq, coeff_eq) in PAIRS {
        let (root_resist, root_react) = sim.read_residual(&format!("implicit_equation_{root_eq}"));
        let (coeff_resist, coeff_react) =
            sim.read_residual(&format!("implicit_equation_{coeff_eq}"));
        // The two sides compute the same coefficients by different routes -- one
        // expands roots, the other reads them out -- so they agree to rounding
        // rather than bit for bit. The residuals reach 1e12, which makes a relative
        // comparison the only meaningful one.
        float_cmp::assert_approx_eq!(f64, root_resist, coeff_resist, ulps = 8);
        float_cmp::assert_approx_eq!(f64, root_react, coeff_react, ulps = 8);
    }

    // The outputs themselves must agree too, not just the internal equations.
    for (root_out, coeff_out) in [("o1", "o2"), ("o3", "o4"), ("o5", "o6"), ("o7", "o8")] {
        let root = sim.read_residual(&format!("flow({root_out})")).0;
        let coeff = sim.read_residual(&format!("flow({coeff_out})")).0;
        float_cmp::assert_approx_eq!(f64, root, coeff, ulps = 8);
    }

    Ok(())
}

/// VAMS-2023 4.5.9: the rate-limited form of `slew` is realized as an implicit
/// equation, the path the `mir` snapshot (lowered without equations) cannot reach.
/// The equation is `dx/dt = clamp((expr - x)/eps, max_neg, max_pos)`, lowered as
/// `react = x`, `resist = -rate`, so the resistive residual of the equation node
/// *is* the negated slew rate and can be read directly. See `slew.va`.
fn test_slew() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    const MAX_POS: f64 = 1e6;
    const MAX_NEG: f64 = -2e6;
    // the gain the lowering chases the target with
    const EPS: f64 = 1e-12;

    let desc = test_descriptor(&openvaf_test_data("osdi").join("slew.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    // Evaluate with the input at `v_in` and the slew state at `x`, and return the
    // (resistive, reactive) residual of the slew equation.
    let eval = |instance: &OsdiInstance,
                model: &OsdiModel,
                sim: &mut MockSimulation,
                v_in: f64,
                x: f64,
                first: bool| {
        if !first {
            sim.next_iter();
        }
        sim.set_voltage("in", v_in);
        sim.set_voltage("out", 0.0);
        sim.set_voltage("thru", 0.0);
        sim.set_voltage("implicit_equation_0", x);
        instance.eval(model, sim, EvalFlags::empty());
        instance.load_dae(model, sim);
        sim.read_residual("implicit_equation_0")
    };

    // Input far above the state: the rate saturates at max_pos, and the reactive
    // residual is the state itself (the d/dt term).
    let (resist, react) = eval(&instance, &model, &mut sim, 1.0, 0.0, true);
    float_cmp::assert_approx_eq!(f64, resist, -MAX_POS, epsilon = 1e-6);
    float_cmp::assert_approx_eq!(f64, react, 0.0, epsilon = 1e-9);

    // Input far below the state: it saturates at max_neg, which is a different
    // magnitude, so this also pins that the two limits are not interchanged.
    let (resist, _) = eval(&instance, &model, &mut sim, -1.0, 0.0, false);
    float_cmp::assert_approx_eq!(f64, resist, -MAX_NEG, epsilon = 1e-6);

    // State already at the input: nothing to chase, so the rate is zero.
    let (resist, react) = eval(&instance, &model, &mut sim, 0.5, 0.5, false);
    float_cmp::assert_approx_eq!(f64, resist, 0.0, epsilon = 1e-9);
    float_cmp::assert_approx_eq!(f64, react, 0.5, epsilon = 1e-9);

    // Below the limits the rate is not clamped but follows the difference, which is
    // what makes the state track the input rather than ramp at a fixed slope.
    let delta = 1e-7;
    let (resist, _) = eval(&instance, &model, &mut sim, delta, 0.0, false);
    float_cmp::assert_approx_eq!(f64, resist, -delta / EPS, epsilon = 1e-3);

    // The form with no rates is a plain pass-through. A voltage contribution puts
    // its equation on the branch flow unknown, where it reads as
    // `slew(V(in)) - V(thru)`, and `thru` gets no equation of its own: with
    // `V(thru)` held at 0 the residual is the input itself.
    float_cmp::assert_approx_eq!(f64, sim.read_residual("flow(thru)").0, delta, epsilon = 1e-12);

    Ok(())
}

/// VAMS-2023 9.13: the probabilistic distributions. 9.13.1 requires that `$random`
/// "shall always return the same stream of values given the same initial
/// random_seed", and 9.13.3 fixes which stream by deferring to IEEE 1364 17.9.3 --
/// so matching other simulators is part of being correct, and the reference values
/// below were taken from one. See `rng_stream.va`.
fn test_rng_stream() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    // (kind, seed, [(value, seed after) ...]) per function.
    let cases: &[(f64, f64, &[(f64, f64)])] = &[
        (
            0.0,
            1.0,
            &[
                (-2147414528.0, 69070.0),
                (-1671855048.0, 475628535.0),
                (1129920902.0, -1017563188.0),
                (-1374483364.0, 772999773.0),
                (1730349006.0, -417135238.0),
                (1674352583.0, -473131853.0),
            ],
        ),
        (
            1.0,
            12345.0,
            &[
                (20.0, 852656806.0),
                (90.0, -438629137.0),
                (24.0, 1023442532.0),
                (37.0, 1580485141.0),
            ],
        ),
        (
            2.0,
            12345.0,
            &[
                (50.0, -438629137.0),
                (37.0, 1580485141.0),
                (49.0, -205612757.0),
                (48.0, 1055483825.0),
            ],
        ),
        (
            3.0,
            7.0,
            &[
                (45.0, 483484.0),
                (1.0, -965981971.0),
                (2.0, -1386778934.0),
                (2.0, -1368524349.0),
            ],
        ),
        (
            4.0,
            7.0,
            &[
                (0.0, 483484.0),
                (6.0, 40936767.0),
                (1.0, 31362789.0),
                (2.0, 478906240.0),
            ],
        ),
        (
            5.0,
            99.0,
            &[
                (6.0, 471364482.0),
                (1.0, -269253343.0),
                (7.0, 239310840.0),
                (2.0, 1843486031.0),
            ],
        ),
        (
            6.0,
            99.0,
            &[
                (-1.0, -1311147616.0),
                (-1.0, 1926252953.0),
                (-1.0, 1221288882.0),
                (1.0, -1063882643.0),
            ],
        ),
        (
            7.0,
            31.0,
            &[
                (25.0, 1857510597.0),
                (4.0, -1344686245.0),
                (4.0, 2141936993.0),
                (9.0, 617986135.0),
            ],
        ),
    ];

    for &(kind, seed0, expected) in cases {
        let desc = test_descriptor(&openvaf_test_data("osdi").join("rng_stream.va"))?;
        let model = desc.new_model();
        // Parameter order is $mfactor, seed0, kind -- see rng_stream.snap.
        model.set_real_param(1, seed0);
        model.set_real_param(2, kind);
        model.process_params()?;
        let mut instance = model.new_instance();
        let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

        for (step, &(want_val, want_seed)) in expected.iter().enumerate() {
            if step != 0 {
                // Only an accepted step advances the seed, which is what makes a
                // draw stable across the Newton iterations of one timestep.
                sim.next_iter();
                sim.advance_time(1e-6);
            }
            sim.set_voltage("in", 0.0);
            sim.set_voltage("val", 0.0);
            sim.set_voltage("seed_out", 0.0);
            instance.eval(&model, &mut sim, EvalFlags::empty());
            instance.load_dae(&model, &mut sim);

            let got_val = sim.read_residual("flow(val)").0;
            let got_seed = sim.read_residual("flow(seed_out)").0;
            assert!(
                (got_val - want_val).abs() < 1e-6,
                "kind {kind} draw {step}: value {got_val}, want {want_val}"
            );
            assert!(
                (got_seed - want_seed).abs() < 1e-6,
                "kind {kind} draw {step}: seed {got_seed}, want {want_seed}"
            );
        }
    }

    // The `$rdist_*` forms, written with integer literals in `Val(Real)`
    // parameters -- the spelling the LRM examples use, and the one that used to
    // reach constant propagation with an int-to-real cast over an already-real
    // value and abort the compiler.
    //
    // They get no reference values of their own on purpose. Each draws the same
    // number as the `$dist_*` kind seven below it and leaves it unrounded, so the
    // table above is the ground truth for these too: round half away from zero,
    // which is what 9.13.1 asks the integer forms to do, and the two must agree
    // on the value and on the seed they leave behind. A number this test made up
    // itself would prove nothing about either.
    let round_half_away = |x: f64| if x < 0.0 { (x - 0.5).trunc() } else { (x + 0.5).trunc() };

    for &(kind, seed0, expected) in &cases[2..] {
        let rkind = kind + 7.0;
        let desc = test_descriptor(&openvaf_test_data("osdi").join("rng_stream.va"))?;
        let model = desc.new_model();
        model.set_real_param(1, seed0);
        model.set_real_param(2, rkind);
        model.process_params()?;
        let mut instance = model.new_instance();
        let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

        for (step, &(want_val, want_seed)) in expected.iter().enumerate() {
            if step != 0 {
                sim.next_iter();
                sim.advance_time(1e-6);
            }
            sim.set_voltage("in", 0.0);
            sim.set_voltage("val", 0.0);
            sim.set_voltage("seed_out", 0.0);
            instance.eval(&model, &mut sim, EvalFlags::empty());
            instance.load_dae(&model, &mut sim);

            let got_val = sim.read_residual("flow(val)").0;
            let got_seed = sim.read_residual("flow(seed_out)").0;
            assert!(
                (round_half_away(got_val) - want_val).abs() < 1e-6,
                "kind {rkind} draw {step}: {got_val} rounds to {}, but kind {kind} drew {want_val}",
                round_half_away(got_val)
            );
            assert!(
                (got_seed - want_seed).abs() < 1e-6,
                "kind {rkind} draw {step}: seed {got_seed}, want {want_seed}"
            );
        }
    }

    // `$rdist_uniform` has no integer partner to check against: `$dist_uniform`
    // draws over the integers in the interval rather than rounding this one. All
    // that is asserted is that it compiles, draws inside the interval it was
    // given, and moves its seed -- enough to catch the crash, which is what this
    // arm is here for.
    {
        let desc = test_descriptor(&openvaf_test_data("osdi").join("rng_stream.va"))?;
        let model = desc.new_model();
        model.set_real_param(1, 12345.0);
        model.set_real_param(2, 8.0);
        model.process_params()?;
        let mut instance = model.new_instance();
        let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

        let mut seeds = Vec::new();
        for step in 0..4 {
            if step != 0 {
                sim.next_iter();
                sim.advance_time(1e-6);
            }
            sim.set_voltage("in", 0.0);
            sim.set_voltage("val", 0.0);
            sim.set_voltage("seed_out", 0.0);
            instance.eval(&model, &mut sim, EvalFlags::empty());
            instance.load_dae(&model, &mut sim);

            let got_val = sim.read_residual("flow(val)").0;
            assert!(
                (0.0..=100.0).contains(&got_val),
                "kind 8 draw {step}: {got_val} is outside the interval 0..100 it was given"
            );
            seeds.push(sim.read_residual("flow(seed_out)").0);
        }
        assert!(
            seeds.windows(2).any(|w| w[0] != w[1]),
            "kind 8: the seed never moved across four accepted steps: {seeds:?}"
        );
    }

    Ok(())
}

/// VAMS-2023 5.10.3.3: `timer` fires at `start_time` and every `period` after it,
/// and a period of zero or less fires once. See `timer_detect.va`.
///
/// The mock simulator does not act on `bound_step`, so the steps here land on the
/// event times by construction; `sim_regression` covers the part where the solver
/// has to be steered onto them.
fn test_timer_detect() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let run = |period: f64, steps: usize| -> Result<Vec<(f64, f64)>> {
        let desc = test_descriptor(&openvaf_test_data("osdi").join("timer_detect.va"))?;
        let model = desc.new_model();
        // Parameter order is $mfactor, start, period -- see timer_detect.snap.
        model.set_real_param(2, period);
        model.process_params()?;
        let mut instance = model.new_instance();
        let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

        let mut out = Vec::new();
        for step in 0..steps {
            if step != 0 {
                sim.next_iter();
                sim.advance_time(1e-6);
            }
            // A distinct input per step, so the sampled value names the step it came
            // from: 0, 1, 2 ... at t = 0, 1 us, 2 us ...
            sim.set_voltage("in", step as f64);
            sim.set_voltage("held", 0.0);
            sim.set_voltage("count", 0.0);
            instance.eval(&model, &mut sim, EvalFlags::empty());
            instance.load_dae(&model, &mut sim);
            out.push((sim.read_residual("flow(held)").0, sim.read_residual("flow(count)").0));
        }
        Ok(out)
    };

    // start = 2 us, period = 2 us: events at 2 us and 4 us, nothing before or between.
    let periodic = run(2e-6, 6)?;
    let expected = [(0.0, 0.0), (0.0, 0.0), (2.0, 1.0), (2.0, 1.0), (4.0, 2.0), (4.0, 2.0)];
    for (got, want) in periodic.iter().zip(expected) {
        float_cmp::assert_approx_eq!(f64, got.0, want.0, epsilon = 1e-9);
        float_cmp::assert_approx_eq!(f64, got.1, want.1, epsilon = 1e-9);
    }

    // "If the period expression evaluates to a value less than or equal to 0.0, the
    // timer shall trigger only once at the specified start_time."
    let once = run(0.0, 6)?;
    let expected = [(0.0, 0.0), (0.0, 0.0), (2.0, 1.0), (2.0, 1.0), (2.0, 1.0), (2.0, 1.0)];
    for (got, want) in once.iter().zip(expected) {
        float_cmp::assert_approx_eq!(f64, got.0, want.0, epsilon = 1e-9);
        float_cmp::assert_approx_eq!(f64, got.1, want.1, epsilon = 1e-9);
    }

    Ok(())
}

/// VAMS-2023: analog block variables keep their value between evaluations, so a
/// read can precede the statement that assigns it and pick up the previous
/// evaluation's value. See `var_persistence.va` for the three shapes checked here.
fn test_var_persistence() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let desc = test_descriptor(&openvaf_test_data("osdi").join("var_persistence.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    let mut step = |instance: &OsdiInstance,
                    model: &OsdiModel,
                    sim: &mut MockSimulation,
                    v_smpl: f64,
                    first: bool| {
        if !first {
            sim.next_iter();
            sim.advance_time(1e-6);
        }
        sim.set_voltage("smpl", v_smpl);
        sim.set_voltage("count", 0.0);
        sim.set_voltage("before", 0.0);
        sim.set_voltage("once", 0.0);
        instance.eval(model, sim, EvalFlags::empty());
        instance.load_dae(model, sim);
        (
            sim.read_residual("flow(count)").0,
            sim.read_residual("flow(before)").0,
            sim.read_residual("flow(once)").0,
        )
    };

    // t = 0. The initializer runs here; nothing has crossed.
    let (n, before, gain) = step(&instance, &model, &mut sim, 0.0, true);
    float_cmp::assert_approx_eq!(f64, n, 0.0, epsilon = 1e-9);
    float_cmp::assert_approx_eq!(f64, before, 0.0, epsilon = 1e-9);
    float_cmp::assert_approx_eq!(f64, gain, 2.0, epsilon = 1e-9);

    // t = 1 us, first crossing. The counter accumulates from its own retained value,
    // and `prev_stamp` picks up `stamp` as it was at the end of the t = 0 evaluation.
    let (n, before, gain) = step(&instance, &model, &mut sim, 5.0, false);
    float_cmp::assert_approx_eq!(f64, n, 1.0, epsilon = 1e-9);
    float_cmp::assert_approx_eq!(f64, before, 0.0, epsilon = 1e-9);
    // The initializer must not be re-applied, but retention must carry it.
    float_cmp::assert_approx_eq!(f64, gain, 2.0, epsilon = 1e-9);

    // t = 2 us, no crossing: both held.
    let (n, before, _) = step(&instance, &model, &mut sim, 5.0, false);
    float_cmp::assert_approx_eq!(f64, n, 1.0, epsilon = 1e-9);
    float_cmp::assert_approx_eq!(f64, before, 0.0, epsilon = 1e-9);

    // t = 3 us, clock low. Falling edges are not events for `dir = +1`.
    let (n, before, _) = step(&instance, &model, &mut sim, 0.0, false);
    float_cmp::assert_approx_eq!(f64, n, 1.0, epsilon = 1e-9);
    float_cmp::assert_approx_eq!(f64, before, 0.0, epsilon = 1e-9);

    // t = 4 us, second crossing. `prev_stamp` now takes the stamp from t = 3 us --
    // the previous evaluation -- not this one.
    let (n, before, gain) = step(&instance, &model, &mut sim, 5.0, false);
    float_cmp::assert_approx_eq!(f64, n, 2.0, epsilon = 1e-9);
    float_cmp::assert_approx_eq!(f64, before, 3e-6, epsilon = 1e-15);
    float_cmp::assert_approx_eq!(f64, gain, 2.0, epsilon = 1e-9);

    Ok(())
}

/// VAMS-2023 4.5.10: `last_crossing` interpolates the zero crossing linearly
/// between the two straddling points, reports a negative value until the expression
/// has crossed, and honours the direction argument.
///
/// The steps here are a whole microsecond apart with values chosen so the exact
/// answer is a round number: from -1 to +1 across 1 us crosses at the midpoint.
/// See `last_crossing.va`; the residual on the branch flow unknown is the reading.
fn test_last_crossing() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let desc = test_descriptor(&openvaf_test_data("osdi").join("last_crossing.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    let mut step = |instance: &OsdiInstance,
                    model: &OsdiModel,
                    sim: &mut MockSimulation,
                    v_in: f64,
                    first: bool| {
        if !first {
            sim.next_iter();
            sim.advance_time(1e-6);
        }
        sim.set_voltage("in", v_in);
        sim.set_voltage("out", 0.0);
        instance.eval(model, sim, EvalFlags::empty());
        instance.load_dae(model, sim);
        sim.read_residual("flow(out)").0
    };

    // "Before the expression crosses zero (0) for the first time, the
    // last_crossing() function returns a negative value."
    let lc = step(&instance, &model, &mut sim, -1.0, true);
    assert!(lc < 0.0, "expected a negative value before the first crossing, got {lc}");

    // t = 1 us, still below zero: nothing has crossed.
    let lc = step(&instance, &model, &mut sim, -1.0, false);
    assert!(lc < 0.0, "expected a negative value before the first crossing, got {lc}");

    // t = 2 us at +1, from -1 at t = 1 us: the crossing is exactly halfway.
    let lc = step(&instance, &model, &mut sim, 1.0, false);
    float_cmp::assert_approx_eq!(f64, lc, 1.5e-6, epsilon = 1e-15);

    // t = 3 us, no crossing: the reading holds.
    let lc = step(&instance, &model, &mut sim, 3.0, false);
    float_cmp::assert_approx_eq!(f64, lc, 1.5e-6, epsilon = 1e-15);

    // t = 4 us, falling through zero. The direction argument is +1, so this is not
    // a crossing as far as this call is concerned.
    let lc = step(&instance, &model, &mut sim, -1.0, false);
    float_cmp::assert_approx_eq!(f64, lc, 1.5e-6, epsilon = 1e-15);

    // t = 5 us at +1, from -1 at t = 4 us: halfway again.
    let lc = step(&instance, &model, &mut sim, 1.0, false);
    float_cmp::assert_approx_eq!(f64, lc, 4.5e-6, epsilon = 1e-15);

    Ok(())
}

/// VAMS-2023 5.10.3.2: `above` also fires during initialization and dc when the
/// expression is already positive, which is the whole reason it exists -- the LRM's
/// own wording is that `cross` "would never trigger, even if the voltage on the smpl
/// port is always above 2.5V". Afterwards it behaves like a rising-only `cross`.
///
/// See `above_detect.va`; the residual on the branch flow unknown is the held value.
fn test_above_detect() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let desc = test_descriptor(&openvaf_test_data("osdi").join("above_detect.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    // The initialization event is keyed on `$abstime` still being zero, not on the
    // analysis flags: ngspice reports ANALYSIS_STATIC only on the first Newton
    // iteration of the initial step, so a flag-gated event fires on one iteration
    // and is overwritten by the others. The flags are left empty here so the test
    // cannot pass by being handed a flag a real simulator would not hold steady.
    let mut step = |instance: &OsdiInstance,
                    model: &OsdiModel,
                    sim: &mut MockSimulation,
                    v_in: f64,
                    v_smpl: f64,
                    first: bool| {
        if !first {
            sim.next_iter();
            sim.advance_time(1e-6);
        }
        sim.set_voltage("in", v_in);
        sim.set_voltage("smpl", v_smpl);
        sim.set_voltage("out", 0.0);
        instance.eval(model, sim, EvalFlags::empty());
        instance.load_dae(model, sim);
        sim.read_residual("flow(out)").0
    };

    // The initial solve at t = 0, with the clock already high: `above` samples here.
    // This is the case `cross` cannot cover -- there is nothing to cross from.
    let held = step(&instance, &model, &mut sim, 1.5, 5.0, true);
    float_cmp::assert_approx_eq!(f64, held, 1.5, epsilon = 1e-9);

    // Time has moved now, clock still high and never crossing: no new event, so the
    // value sampled at initialization is held even though the input moved.
    let held = step(&instance, &model, &mut sim, 2.5, 5.0, false);
    float_cmp::assert_approx_eq!(f64, held, 1.5, epsilon = 1e-9);

    // Clock drops. `above` has no `dir` argument and triggers only from below, so a
    // falling edge is not an event.
    let held = step(&instance, &model, &mut sim, 3.5, 0.0, false);
    float_cmp::assert_approx_eq!(f64, held, 1.5, epsilon = 1e-9);

    // Clock rises through the threshold: that is an event, like `cross`.
    let held = step(&instance, &model, &mut sim, 4.5, 5.0, false);
    float_cmp::assert_approx_eq!(f64, held, 4.5, epsilon = 1e-9);

    Ok(())
}

/// VAMS-2023 5.10.3.1: `cross` fires once per upward threshold crossing, so the
/// sampled value is held between crossings instead of following the input.
///
/// The detector compares the expression against its value at the previous
/// *accepted* timestep, which is what `next_iter` models here. See
/// `cross_detect.va`.
fn test_cross_detect() -> Result<()> {
    if stdx::IS_CI && cfg!(windows) {
        return Ok(());
    }

    let desc = test_descriptor(&openvaf_test_data("osdi").join("cross_detect.va"))?;
    let model = desc.new_model();
    model.process_params()?;
    let mut instance = model.new_instance();
    let mut sim = instance.mock_simulation(&model, desc.num_terminals, 300.0)?;

    // One accepted timestep: commit the previous state, advance time, apply the
    // inputs, evaluate. The residual on the branch flow unknown is the held value.
    let mut step = |instance: &OsdiInstance,
                    model: &OsdiModel,
                    sim: &mut MockSimulation,
                    v_in: f64,
                    v_smpl: f64,
                    first: bool| {
        if !first {
            sim.next_iter();
        }
        sim.advance_time(1e-6);
        sim.set_voltage("in", v_in);
        sim.set_voltage("smpl", v_smpl);
        sim.set_voltage("out", 0.0);
        instance.eval(model, sim, EvalFlags::empty());
        instance.load_dae(model, sim);
        sim.read_residual("flow(out)").0
    };

    // Below the threshold: nothing sampled yet.
    let held = step(&instance, &model, &mut sim, 1.0, 0.0, true);
    float_cmp::assert_approx_eq!(f64, held, 0.0, epsilon = 1e-9);

    // Cross upward: samples v(in) = 2.0.
    let held = step(&instance, &model, &mut sim, 2.0, 5.0, false);
    float_cmp::assert_approx_eq!(f64, held, 2.0, epsilon = 1e-9);

    // Still high, no new crossing: the input moved but the held value must not.
    let held = step(&instance, &model, &mut sim, 3.0, 5.0, false);
    float_cmp::assert_approx_eq!(f64, held, 2.0, epsilon = 1e-9);

    // Falling edge, and `dir` is +1, so this is not an event either.
    let held = step(&instance, &model, &mut sim, 4.0, 0.0, false);
    float_cmp::assert_approx_eq!(f64, held, 2.0, epsilon = 1e-9);

    // Cross upward again: samples the new input.
    let held = step(&instance, &model, &mut sim, 5.0, 5.0, false);
    float_cmp::assert_approx_eq!(f64, held, 5.0, epsilon = 1e-9);

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
    [Test::new("$limit", &test_limit),Test::new("noise", &test_noise),Test::new("arrays", &test_arrays),Test::new("cross_latch", &test_cross_latch),Test::new("laplace_nd_int", &test_laplace_nd_int),Test::new("vector_ports", &test_vector_ports),Test::new("qam16", &test_qam16),Test::new("cross_array", &test_cross_array),Test::new("adc", &test_adc),Test::new("indirect_opamp", &test_indirect_opamp),Test::new("laplace_null_zeros", &test_laplace_null_zeros),Test::new("laplace_roots", &test_laplace_roots),Test::new("slew", &test_slew),Test::new("cross_detect", &test_cross_detect),Test::new("above_detect", &test_above_detect),Test::new("last_crossing", &test_last_crossing),Test::new("var_persistence", &test_var_persistence),Test::new("timer_detect", &test_timer_detect),Test::new("rng_stream", &test_rng_stream)]
}
