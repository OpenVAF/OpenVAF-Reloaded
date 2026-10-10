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
/* VAMS-2023 9.5. Declared rather than included for the same reason as the rest
 * of this block: the file is compiled freestanding, and the library it ends up
 * in is loaded into a process that has a libc. */
extern void free(void *);
extern void *fopen(const char *, const char *);
extern int fclose(void *);
extern int fputs(const char *, void *);
extern int fflush(void *);
extern int feof(void *);
extern long ftell(void *);
extern int fseek(void *, long, int);
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

/* --- VAMS-2023 9.5: files --------------------------------------------------
 *
 * 9.5.1 keeps two kinds of descriptor in one integer, and which kind a given
 * one is lives in the value: `$fopen(name)` returns a *multichannel* descriptor
 * with a single bit set, `$fopen(name, mode)` a *file* descriptor with the top
 * bit set and a small index below it. That is why every output task takes one
 * integer and works for either, and why an mcd can name several files at once --
 * `$fdisplay(mcd1 | mcd2, ...)` writes to both.
 *
 * Channel 0 of an mcd, and the three reserved file descriptors, are standard
 * input, output and error. A model inside a simulator has no business writing to
 * the process's stdout, and the simulator already has somewhere for a model's
 * words to go, so those route to `osdi_log` -- the same place `$display` ends up.
 *
 * The tables are per loaded library rather than per instance, which is what the
 * clause describes: a descriptor is an integer, so passing one between instances
 * (or storing it in a parameter) has to mean the same file to both.
 */

#define VA_FD_BIT 0x80000000u
#define VA_NMCD 31
#define VA_NFD 32
/* stdin, stdout, stderr as 9.5.1 numbers them. */
#define VA_FD_RESERVED 3

static void *va_mcd[VA_NMCD];
static void *va_fd[VA_NFD];
/* What each open descriptor was opened as: the mode and the name, so that a
 * second request for the same pair can be answered with the same descriptor.
 *
 * 5.10.2 puts `@(initial_step)` in force "during the solution of the first
 * point", which is every Newton iteration of it, not just the first -- so the one
 * place a model can open a file once is a place it is asked to open it several
 * times. Opening it again would truncate what the earlier iterations wrote and
 * hand out a descriptor that nothing is holding, until the thirty of them run
 * out. The clause says nothing either way; this is the reading that lets the
 * idiom work. The cost is that a model wanting two independent handles on one
 * file gets one, which is not something 9.5 offers a way to ask for.
 */
static char *va_mcd_key[VA_NMCD];
static char *va_fd_key[VA_NFD];

/* "shall return zero if the file could not be opened" -- for both forms, which
 * is also why channel 0 is never handed out: its bit set alone would be 1. */
int32_t va_fopen(const char *name, const char *mode) {
  void *f;
  char *key;
  if (mode == NULL || name == NULL) {
    return 0;
  }
  key = concat(mode, name);
  if (key == NULL) {
    return 0;
  }
  for (uint32_t i = VA_FD_RESERVED; i < VA_NFD; i++) {
    if (va_fd[i] != NULL && va_fd_key[i] != NULL && strcmp(va_fd_key[i], key) == 0) {
      free(key);
      return (int32_t)(VA_FD_BIT | i);
    }
  }
  f = fopen(name, mode);
  if (f == NULL) {
    free(key);
    return 0;
  }
  for (uint32_t i = VA_FD_RESERVED; i < VA_NFD; i++) {
    if (va_fd[i] == NULL) {
      va_fd[i] = f;
      va_fd_key[i] = key;
      return (int32_t)(VA_FD_BIT | i);
    }
  }
  fclose(f);
  free(key);
  return 0;
}

int32_t va_fopen_mcd(const char *name) {
  void *f;
  char *key;
  if (name == NULL) {
    return 0;
  }
  /* No mode: 9.5.1 opens a multichannel descriptor for writing. */
  key = concat("w", name);
  if (key == NULL) {
    return 0;
  }
  for (uint32_t b = 1; b < VA_NMCD; b++) {
    if (va_mcd[b] != NULL && va_mcd_key[b] != NULL && strcmp(va_mcd_key[b], key) == 0) {
      free(key);
      return (int32_t)(1u << b);
    }
  }
  f = fopen(name, "w");
  if (f == NULL) {
    free(key);
    return 0;
  }
  for (uint32_t b = 1; b < VA_NMCD; b++) {
    if (va_mcd[b] == NULL) {
      va_mcd[b] = f;
      va_mcd_key[b] = key;
      return (int32_t)(1u << b);
    }
  }
  fclose(f);
  free(key);
  return 0;
}

/* A file descriptor names one file; a multichannel descriptor names every
 * channel whose bit is set, so closing one closes all of them. */
int32_t va_fclose(int32_t desc) {
  uint32_t d = (uint32_t)desc;
  if (d & VA_FD_BIT) {
    uint32_t i = d & ~VA_FD_BIT;
    if (i >= VA_FD_RESERVED && i < VA_NFD && va_fd[i] != NULL) {
      fclose(va_fd[i]);
      va_fd[i] = NULL;
      free(va_fd_key[i]);
      va_fd_key[i] = NULL;
    }
    return 0;
  }
  for (uint32_t b = 1; b < VA_NMCD; b++) {
    if (((d >> b) & 1) && va_mcd[b] != NULL) {
      fclose(va_mcd[b]);
      va_mcd[b] = NULL;
      free(va_mcd_key[b]);
      va_mcd_key[b] = NULL;
    }
  }
  return 0;
}

/* The one file a descriptor names, for the operations that only make sense on
 * one: NULL for a reserved descriptor, a closed one, or an mcd naming several. */
static void *va_one_file(int32_t desc) {
  uint32_t d = (uint32_t)desc;
  if (d & VA_FD_BIT) {
    uint32_t i = d & ~VA_FD_BIT;
    if (i < VA_FD_RESERVED || i >= VA_NFD) {
      return NULL;
    }
    return va_fd[i];
  }
  for (uint32_t b = 1; b < VA_NMCD; b++) {
    if ((d >> b) & 1) {
      /* Only if it is the only bit set. */
      if ((d & ~(1u << b) & ~1u) != 0) {
        return NULL;
      }
      return va_mcd[b];
    }
  }
  return NULL;
}

void va_fputs(void *handle, int32_t desc, char *msg, uint32_t lvl) {
  uint32_t d = (uint32_t)desc;
  if (d & VA_FD_BIT) {
    uint32_t i = d & ~VA_FD_BIT;
    if (i < VA_FD_RESERVED) {
      osdi_log(handle, msg, lvl);
    } else if (i < VA_NFD && va_fd[i] != NULL) {
      fputs(msg, va_fd[i]);
    }
    return;
  }
  /* Channel 0 is the simulator's own output. */
  if (d & 1) {
    osdi_log(handle, msg, lvl);
  }
  for (uint32_t b = 1; b < VA_NMCD; b++) {
    if (((d >> b) & 1) && va_mcd[b] != NULL) {
      fputs(msg, va_mcd[b]);
    }
  }
}

/* 9.5.6: "$fflush(mcd) ... writes any buffered output"; with no argument it
 * flushes everything this library has open. */
int32_t va_fflush(int32_t desc, int32_t all) {
  if (all) {
    for (uint32_t i = VA_FD_RESERVED; i < VA_NFD; i++) {
      if (va_fd[i] != NULL) {
        fflush(va_fd[i]);
      }
    }
    for (uint32_t b = 1; b < VA_NMCD; b++) {
      if (va_mcd[b] != NULL) {
        fflush(va_mcd[b]);
      }
    }
    return 0;
  }
  {
    uint32_t d = (uint32_t)desc;
    if (d & VA_FD_BIT) {
      void *f = va_one_file(desc);
      if (f != NULL) {
        fflush(f);
      }
      return 0;
    }
    for (uint32_t b = 1; b < VA_NMCD; b++) {
      if (((d >> b) & 1) && va_mcd[b] != NULL) {
        fflush(va_mcd[b]);
      }
    }
  }
  return 0;
}

int32_t va_feof(int32_t desc) {
  void *f = va_one_file(desc);
  /* A descriptor that names no file is at its end as much as it is anywhere. */
  return f == NULL ? 1 : (feof(f) != 0 ? 1 : 0);
}

int32_t va_ftell(int32_t desc) {
  void *f = va_one_file(desc);
  return f == NULL ? -1 : (int32_t)ftell(f);
}

int32_t va_fseek(int32_t desc, int32_t offset, int32_t operation) {
  void *f = va_one_file(desc);
  return f == NULL ? -1 : (int32_t)fseek(f, (long)offset, (int)operation);
}

int32_t va_rewind(int32_t desc) {
  void *f = va_one_file(desc);
  if (f == NULL) {
    return -1;
  }
  return (int32_t)fseek(f, 0, 0 /* SEEK_SET */);
}

/* True the first time it is called at a given value and false while the value
 * stays the same.
 *
 * The cell is the uncommitted half of a retained slot, which is written as soon
 * as it is assigned rather than when a timestep is accepted -- so passing
 * `$abstime` distinguishes the first evaluation at a timepoint from the Newton
 * iterations that follow it, which is what "at the end of the current simulation
 * time" needs in order not to mean "once per iteration". It is per instance,
 * because the slot is.
 */
double retained_first(double *dst, double val) {
  if (*dst == val) {
    return 0.0;
  }
  *dst = val;
  return 1.0;
}

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
