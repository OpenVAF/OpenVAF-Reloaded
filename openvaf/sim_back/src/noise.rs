use lasso::Spur;
use mir::Value;
use stdx::Ieee64;

use crate::dae::SimUnknown;

#[derive(Debug, Clone)]
pub enum NoiseSourceKind {
    WhiteNoise {
        pwr: Value,
    },
    FlickerNoise {
        pwr: Value,
        exp: Value,
    },
    NoiseTable {
        log: bool,
        vals: Box<[(Ieee64, Ieee64)]>,
    },
    /// The same table, with its `(frequency, power)` pairs read at run time
    /// instead of built into the model: a spectrum a model card can set.
    ///
    /// `log` means what it does for [`NoiseSourceKind::NoiseTable`], except that
    /// nothing has been pre-computed: a linear table's frequencies are logged
    /// where they are read. The pairs keep the order the array declares them in.
    NoiseTableDyn {
        log: bool,
        pairs: Box<[(Value, Value)]>,
    },
}

#[derive(Debug)]
pub struct NoiseSource {
    pub name: Spur,
    pub kind: NoiseSourceKind,
    pub hi: SimUnknown,
    pub lo: Option<SimUnknown>,
    pub factor: Value,
}

impl NoiseSource {
    pub fn map_vals(&mut self, mut f: impl FnMut(Value) -> Value) {
        self.factor = f(self.factor);
        match &mut self.kind {
            NoiseSourceKind::WhiteNoise { pwr } => *pwr = f(*pwr),
            NoiseSourceKind::FlickerNoise { pwr, exp } => {
                *pwr = f(*pwr);
                *exp = f(*exp);
            }
            NoiseSourceKind::NoiseTable { .. } => (),
            NoiseSourceKind::NoiseTableDyn { pairs, .. } => {
                for (freq, pwr) in pairs.iter_mut() {
                    *freq = f(*freq);
                    *pwr = f(*pwr);
                }
            }
        }
    }
}
