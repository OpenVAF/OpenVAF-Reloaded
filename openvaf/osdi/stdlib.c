#ifdef NO_STD
// This was used before. Seems wrong. AB
// typedef int int32_t;
// Maybe this is better... AB
typedef unsigned int uint32_t;
typedef int int32_t;
// End of change AB
typedef unsigned char bool;
typedef __SIZE_TYPE__ size_t;
extern size_t strlen (const char *__s);
extern void *memcpy (void *__restrict __dest, const void *__restrict __src,
		     size_t __n);
extern void *malloc (size_t __size);
extern void *realloc (void *__ptr, size_t __size);
extern double log(double);
extern double exp(double);
extern double sqrt(double);
extern int strcmp(const char*, const char*);
#define NULL ((void*)0)
#else
#include <math.h>
#include <stdio.h>
#include "stdlib.h"
#include "string.h"
#endif

#ifndef OSDI_0_4
#include "header/osdi_0_4.h"
#endif

// no header was included explicitly so just use the newest version
#ifndef OSDI_VERSION_MAJOR_CURR
#include "header/osdi_0_4.h"
#endif


char *concat(const char *s1, const char *s2) {
  const size_t len1 = strlen(s1);
  const size_t len2 = strlen(s2);
  char *result = malloc(len1 + len2 + 1);
  if (result == NULL) {
    return NULL;
  }
  memcpy(result, s1, len1);
  memcpy(result + len1, s2, len2 + 1);
  return result;
}

typedef void (*osdi_log_ptr)(void *handle, char *msg, uint32_t lvl);
extern osdi_log_ptr osdi_log;

#define SCMP(p1, p2, s1, s2, eq) for(p1=s1, p2=s2;*p1 && *p2 && *p1==*p2;p1++, p2++); eq = (*p1==*p2);

double simparam(void *params_, void *handle, uint32_t *flags, char *name) {
  OsdiSimParas *params = params_;
  for (int i = 0; params->names[i]; i++) {
    char *p1, *p2;
    int eq;
    SCMP(p1, p2, params->names[i], name, eq);
    // if (strcmp(params->names[i], name) == 0) {
    if (eq) {
      return params->vals[i];
    }
  }
  *flags |= EVAL_RET_FLAG_FATAL;
  char *msg = concat("unknown $simparam", name);
  if (msg == NULL) {
    osdi_log(handle, "unknown $simparam %s", LOG_LVL_FATAL | LOG_FMT_ERR);
  } else {
    osdi_log(handle, msg, LOG_LVL_FATAL);
  }
  return 0.0;
}

double simparam_opt(void *params_, char *name, double default_val) {
  OsdiSimParas *params = params_;
  for (int i = 0; params->names[i]; i++) {
    char *p1, *p2;
    int eq;
    SCMP(p1, p2, params->names[i], name, eq);
    // if (strcmp(params->names[i], name) == 0) {
    if (eq) {
      return params->vals[i];
    }
  }
  return default_val;
}

extern int strcmp(const char *__s1, const char *__s2);

char *simparam_str(void *params_, void *handle, uint32_t *flags, char *name) {
  OsdiSimParas *params = params_;
  // Walk the *string* parameter list (`names_str`, NULL-terminated) and return
  // the matching *value* (`vals_str`). Previously this loop iterated using the
  // numeric `names` array as its bound (an out-of-bounds read once the string
  // list is shorter) and returned the name itself instead of the value, so
  // `$simparam$str` never worked.
  if (params->names_str) {
    for (int i = 0; params->names_str[i]; i++) {
      char *p1, *p2;
      int eq;
      SCMP(p1, p2, params->names_str[i], name, eq);
      if (eq) {
        return params->vals_str[i];
      }
    }
  }
  *flags |= EVAL_RET_FLAG_FATAL;

  char *msg = concat("unknown $simparam_str", name);
  if (msg == NULL) {
    osdi_log(handle, "unknown $simparam_str %s", LOG_LVL_FATAL | LOG_FMT_ERR);
  } else {
    osdi_log(handle, msg, LOG_LVL_FATAL);
  }

  return "�";
}

void push_error(OsdiInitError **dst, uint32_t *len, uint32_t *cap,
                OsdiInitError err) {
  if (*dst == NULL) {
    *cap = 8;
    *dst = malloc(8 * sizeof(OsdiInitError));
  } else if (*cap <= *len) {
    *cap = 2 * (*len);
    *dst = realloc(*dst, *cap * sizeof(OsdiInitError));
  }

  (*dst)[*len] = err;
  *len += 1;
}

void push_invalid_param_err(void **dst, uint32_t *len, uint32_t *cap,
                            uint32_t param) {
  OsdiInitError err = (OsdiInitError){
      .code = INIT_ERR_OUT_OF_BOUNDS,
      .payload =
          (OsdiInitErrorPayload){
              .parameter_id = param,
          },
  };

  push_error((OsdiInitError **)dst, len, cap, err);
}

void bound_step(double *dst, double val) { *dst = val; }

#define FMT_OFF 6
#define NUM_FMT 11
const char FMT_CHARS[NUM_FMT] = {'a', 'f', 'p', 'n', 'u', 'm',
                                 ' ', 'k', 'M', 'G', 'T'};
const double EXP[NUM_FMT] = {1e18, 1e15, 1e12, 1e9,  1e6,  1e3,
                             1,    1e-3, 1e-6, 1e-9, 1e-12};
int fmt_char_idx(double val) {
  int exp = ((int)log(val)) / 3;
  int pos = exp + NUM_FMT;

  if (pos < 0) {
    return 0;
  }
  if (pos >= NUM_FMT) {
    return NUM_FMT - 1;
  }
  return pos;
}

char *fmt_binary(int val) {
  int len = 32 - __builtin_clz(val);
  char *res = malloc(len + 1);
  res[len] = '\0';
  if (len == 0) {
    return res;
  }
  for (int i = 1; i < len + 1; i++) {
    if (val & 1) {
      res[len - i] = '1';
    } else {
      res[len - i] = '0';
    }
    val >>= 1;
  }

  return res;
}

void lim_discontinuity(int *flags) { *flags |= EVAL_RET_FLAG_LIM; }

void set_ret_flag_fatal(int *flags) { *flags |= EVAL_RET_FLAG_FATAL; }

void set_ret_flag_finish(int *flags) { *flags |= EVAL_RET_FLAG_FINISH; }

void set_ret_flag_stop(int *flags) { *flags |= EVAL_RET_FLAG_STOP; }

double store_lim(void *sim_info_, int idx, double val) {
  OsdiSimInfo *sim_info = (OsdiSimInfo *)sim_info_;
  sim_info->next_state[idx] = val;
  return val;
}

int analysis(void *sim_info_, char *name) {
  OsdiSimInfo *sim_info = (OsdiSimInfo *)sim_info_;
  uint32_t flags = sim_info->flags;
  // AB: fixed bug, missing ! in front of strcmp()
  return ((flags & ANALYSIS_AC) && !strcmp(name, "ac")) ||
         ((flags & ANALYSIS_DC) && !strcmp(name, "dc")) ||
         ((flags & ANALYSIS_NOISE) && !strcmp(name, "noise")) ||
         ((flags & ANALYSIS_TRAN) && !strcmp(name, "tran")) ||
         ((flags & ANALYSIS_IC) && !strcmp(name, "ic")) ||
         ((flags & ANALYSIS_STATIC) && !strcmp(name, "static")) ||
         ((flags & ANALYSIS_NODESET) && !strcmp(name, "nodeset"));
}

/* Retained state: values that must survive from one accepted timestep to the
 * next, such as the latch behind an `@(cross)` variable or the previous value of
 * a monitored expression.
 *
 * These deliberately do NOT live in the OSDI state array. `prev_state` and
 * `next_state` are the *limiting* state array: they carry a value between Newton
 * iterations, and a simulator is free to alias them (ngspice points both at
 * `CKTstates[0]`, which its integrator also rotates per accepted step, so a model
 * reads back a slot it wrote several steps earlier). That is harmless for
 * `$limit`, where the state only steers the Newton path and never the converged
 * answer, but a retained value *is* the answer.
 *
 * So each slot is a pair of doubles in the instance data, which no simulator
 * rotates, plus one timestamp per instance:
 *
 *   vals[2*i]     committed -- what the model reads as "the previous timestep"
 *   vals[2*i + 1] pending   -- what this timestep's evaluations have written
 *
 * `commit_retained` runs once at the top of every eval and uses `$abstime` to
 * decide what happened since the last call:
 *
 *   abstime >  *time   time moved on, so the evaluations at *time were accepted:
 *                      promote pending to committed.
 *   abstime == *time   another Newton iteration of the same step: leave committed
 *                      alone, so every iteration sees the same previous value.
 *   abstime <  *time   the step at *time was rejected and is being retried with a
 *                      smaller delta: drop pending (the retry overwrites it) and
 *                      keep committed, which still holds the last accepted value.
 *
 * Recording `abstime` unconditionally covers all three. A dc, ac or noise
 * analysis reports abstime = 0 throughout, so nothing is ever committed there.
 */
/* ------------------------------------------------------------------------
 * Probabilistic distributions, VAMS-2023 9.13.
 *
 * 9.13.3 does not spell the algorithm out; it defers to IEEE 1364 subclause
 * 17.9.3, and 9.13.1 requires that "$random shall always return the same stream
 * of values given the same initial random_seed". Matching other simulators is
 * therefore part of being correct, not a nicety, and this is written against
 * that specification and checked number for number against a known-good
 * implementation -- all eight functions, values and advanced seeds alike.
 *
 * The seed crosses the ABI as a double because that is what the retained-state
 * slots hold; it is an int32 the whole way, which a double carries exactly.
 * ---------------------------------------------------------------------- */

#define VA_RNG_UNIFORM_INT 0
#define VA_RNG_UNIFORM 1
#define VA_RNG_NORMAL 2
#define VA_RNG_EXPONENTIAL 3
#define VA_RNG_POISSON 4
#define VA_RNG_CHI_SQUARE 5
#define VA_RNG_T 6
#define VA_RNG_ERLANG 7

/* One step of the multiplicative congruential sequence the standard specifies,
 * over the full 32-bit word. Unsigned, so the wrap is defined rather than the
 * signed overflow the original relied on. Zero is replaced, being a seed the
 * sequence cannot leave usefully. */
static uint32_t va_rng_step(uint32_t seed) {
  return 69069u * (seed ? seed : 259341593u) + 1u;
}

/* A draw in [a, b), advancing the seed. The fraction comes from the top 23 bits
 * of the new seed laid straight into a mantissa over [1, 2), stretched by one
 * ulp before being mapped onto the interval. Reproducing that bit layout is the
 * whole point. */
static double va_rng_uniform(uint32_t *seed, double a, double b) {
  const double ulp = 0.00000011920928955078125; /* 2^-23 */
  uint32_t next = va_rng_step(*seed);
  double c;

  *seed = next;
  if (a >= b) {
    a = 0.0;
    b = 2147483647.0;
  }
  c = 1.0 + (double)(next >> 9) * ulp;
  c += c * ulp;
  return (b - a) * (c - 1.0) + a;
}

/* An inclusive draw over [start, end]. Three cases, so the inclusive end can be
 * represented without running the range past what an int32 holds. */
static double va_rng_uniform_int(uint32_t *seed, double start_d, double end_d) {
  int32_t start = (int32_t)start_d;
  int32_t end = (int32_t)end_d;
  double r;
  int32_t i;

  if (start >= end)
    return (double)start;

  if (end != 2147483647) {
    r = va_rng_uniform(seed, (double)start, (double)end + 1.0);
    i = (r >= 0) ? (int32_t)r : (int32_t)(r - 1);
    if (i < start)
      i = start;
    if (i > end)
      i = end;
    return (double)i;
  }

  if (start != (-2147483647 - 1)) {
    r = va_rng_uniform(seed, (double)start - 1.0, (double)end) + 1.0;
    i = (r >= 0) ? (int32_t)r : (int32_t)(r - 1);
    if (i <= start - 1)
      i = start;
    if (i > end)
      i = end;
    return (double)i;
  }

  r = (va_rng_uniform(seed, (double)start, (double)end) + 2147483648.0) /
      4294967295.0;
  r = r * 4294967296.0 - 2147483648.0;
  return (double)((r >= 0) ? (int32_t)r : (int32_t)(r - 1));
}

/* Marsaglia polar: draw points in the square until one lands inside the unit
 * circle, which leaves the pair jointly normal once scaled. */
static double va_rng_normal(uint32_t *seed, double mean, double deviation) {
  double v1 = 0.0, v2, s = 1.0;
  while (s >= 1.0 || s == 0.0) {
    v1 = va_rng_uniform(seed, -1.0, 1.0);
    v2 = va_rng_uniform(seed, -1.0, 1.0);
    s = v1 * v1 + v2 * v2;
  }
  return v1 * sqrt(-2.0 * log(s) / s) * deviation + mean;
}

static double va_rng_exponential(uint32_t *seed, double mean) {
  double n = va_rng_uniform(seed, 0.0, 1.0);
  return n != 0.0 ? -log(n) * mean : n;
}

static double va_rng_poisson(uint32_t *seed, double mean) {
  int32_t n = 0;
  double p = exp(-mean);
  double q = va_rng_uniform(seed, 0.0, 1.0);
  while (p < q) {
    n++;
    q = va_rng_uniform(seed, 0.0, 1.0) * q;
  }
  return (double)n;
}

/* An odd degree of freedom leaves one squared normal over; the rest pair up into
 * exponentials, which is cheaper than squaring normals. */
static double va_rng_chi_square(uint32_t *seed, double deg_of_free) {
  int32_t df = (int32_t)deg_of_free;
  double x;
  int32_t k;

  if (df % 2) {
    double n = va_rng_normal(seed, 0.0, 1.0);
    x = n * n;
  } else {
    x = 0.0;
  }
  for (k = 2; k <= df; k += 2)
    x += 2.0 * va_rng_exponential(seed, 1.0);
  return x;
}

static double va_rng_t(uint32_t *seed, double deg_of_free) {
  double chi2 = va_rng_chi_square(seed, deg_of_free);
  return va_rng_normal(seed, 0.0, 1.0) / sqrt(chi2 / deg_of_free);
}

static double va_rng_erlang(uint32_t *seed, double k_stage, double mean) {
  int32_t k = (int32_t)k_stage;
  double x = 1.0;
  int32_t i;

  for (i = 1; i <= k; i++)
    x *= va_rng_uniform(seed, 0.0, 1.0);
  return -mean * log(x) / k_stage;
}

static double va_rng_draw(uint32_t *seed, uint32_t kind, double a, double b) {
  switch (kind) {
  case VA_RNG_UNIFORM_INT:
    return va_rng_uniform_int(seed, a, b);
  case VA_RNG_UNIFORM:
    return va_rng_uniform(seed, a, b);
  case VA_RNG_NORMAL:
    return va_rng_normal(seed, a, b);
  case VA_RNG_EXPONENTIAL:
    return va_rng_exponential(seed, a);
  case VA_RNG_POISSON:
    return va_rng_poisson(seed, a);
  case VA_RNG_CHI_SQUARE:
    return va_rng_chi_square(seed, a);
  case VA_RNG_T:
    return va_rng_t(seed, a);
  case VA_RNG_ERLANG:
    return va_rng_erlang(seed, a, b);
  default:
    return 0.0;
  }
}

/* The two halves a caller needs. Both are pure functions of (kind, seed, a, b),
 * so the same call site asked twice gives the same answer: one for the value,
 * one for where the seed ended up. Splitting them keeps the ABI to a single
 * return value without an out-parameter the MIR would have to model. */
double rng_value(uint32_t kind, double seed, double a, double b) {
  uint32_t s = (uint32_t)(int32_t)seed;
  return va_rng_draw(&s, kind, a, b);
}

double rng_seed(uint32_t kind, double seed, double a, double b) {
  uint32_t s = (uint32_t)(int32_t)seed;
  va_rng_draw(&s, kind, a, b);
  /* Back as a signed int32, which is what a Verilog integer seed holds. */
  return (double)(int32_t)s;
}

double store_retained(double *dst, double val) {
  *dst = val;
  return val;
}

void commit_retained(double *vals, double *time, uint32_t num, double abstime) {
  if (abstime > *time) {
    for (uint32_t i = 0; i < num; i++) {
      vals[2 * i] = vals[2 * i + 1];
    }
  }
  *time = abstime;
}

double store_delay(void *sim_info_, double *dst, double val) {
  OsdiSimInfo *sim_info = (OsdiSimInfo *)sim_info_;
  if (sim_info->flags & ANALYSIS_IC) {
    *dst = val;
    return val;
  }

  return *dst;
}
