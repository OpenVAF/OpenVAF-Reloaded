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
extern char *fgets(char *, int, void *);
extern double strtod(const char *, char **);
extern long strtol(const char *, char **, int);
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

/* The state below has to be one object however many times this file is compiled
 * into the library: the generated code is several object files (the evaluation,
 * each setup function, the loads), each of which gets its own copy of the bitcode,
 * and `eval` opening a descriptor that `accept` writes through only works if both
 * see the same table. A weak definition is merged by the linker; a static one, or a
 * tentative definition under -fno-common, is one per object. */
#define VA_SHARED __attribute__((weak))

#define VA_FD_BIT 0x80000000u
#define VA_NMCD 31
#define VA_NFD 32
/* stdin, stdout, stderr as 9.5.1 numbers them. */
#define VA_FD_RESERVED 3

VA_SHARED void *va_mcd[VA_NMCD];
VA_SHARED void *va_fd[VA_NFD];
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
VA_SHARED char *va_mcd_key[VA_NMCD];
VA_SHARED char *va_fd_key[VA_NFD];

/* ------------------------------------------------------------------------
 * Where a read-only descriptor stood when the current timepoint began.
 *
 * 9.5.4's reads are sequential: each one takes the next record. An analog block
 * is evaluated many times for one timepoint, so a read taken literally takes a
 * record per Newton iteration, which is not a rate anything in the clause
 * suggests: a file of one record per timepoint is then consumed two to three
 * times too fast, and a table read inside `@(initial_step)` is read past its end
 * by the iterations of the first point.
 *
 * So each read-only descriptor remembers where it stood when the point began, and
 * every evaluation of that point starts its reads there. The point's reads are
 * what advances the file, once, and they advance it by however much the model
 * read:
 *
 *   `va_read_begin`  at the top of every evaluation: back to the mark.
 *   `va_accept`      the point was accepted, so its reads stand: mark = here.
 *   no accept call   the mark follows `$abstime` moving forward, which is the
 *                    same thing for every point the solver does not go back on.
 *
 * Only a read-only descriptor is managed. A mode with `+` in it is a model mixing
 * reads and writes, which has to position the stream itself between the two
 * anyway (C99 7.19.5.3), and moving it from under such a model would be a
 * surprise rather than a service.
 * ---------------------------------------------------------------------- */
VA_SHARED long va_fd_mark[VA_NFD];
VA_SHARED double va_read_time;
VA_SHARED bool va_read_time_set;

/* Set by the first `accept` call; defined with the strobe lines it also governs.
 * Everything that happens once per accepted point shares it, so that a library
 * whose simulator makes the call never also infers one from `$abstime`. */
extern bool va_accept_seen;

/* A descriptor whose reads this manages: opened for reading and nothing else. The
 * key is the mode followed by the name, so the mode is its leading characters. */
static bool va_fd_read_only(uint32_t i) {
  const char *mode = va_fd_key[i];
  if (va_fd[i] == NULL || mode == NULL || mode[0] != 'r') {
    return 0;
  }
  for (; *mode != '\0'; mode++) {
    if (*mode == '+') {
      return 0;
    }
  }
  return 1;
}

/* Every evaluation's reads start where the point's did. `abstime` is the point. */
void va_read_begin(double abstime) {
  uint32_t i;
  /* Without an `accept` call, `$abstime` says what happened since the last one,
   * and it is read exactly as `commit_retained` reads it: time moved on, so the
   * reads of the point just left stand; time went backwards, so that point was
   * rejected, and keeping the mark where it is undoes its reads; the same time
   * again is another iteration of one point, which the seek below takes care of.
   *
   * The two have to agree, because a model's retained state and its file position
   * are one state: a value kept from a record the file has moved past is a source
   * that has lost its place.
   */
  bool advance = !va_accept_seen && (!va_read_time_set || abstime > va_read_time);
  for (i = VA_FD_RESERVED; i < VA_NFD; i++) {
    if (!va_fd_read_only(i)) {
      continue;
    }
    if (advance) {
      va_fd_mark[i] = ftell(va_fd[i]);
    }
    if (va_fd_mark[i] >= 0 && ftell(va_fd[i]) != va_fd_mark[i]) {
      fseek(va_fd[i], va_fd_mark[i], 0 /* SEEK_SET */);
    }
  }
  va_read_time = abstime;
  va_read_time_set = 1;
}

/* The point was accepted, so what it read is read. */
static void va_read_commit(void) {
  uint32_t i;
  for (i = VA_FD_RESERVED; i < VA_NFD; i++) {
    if (va_fd_read_only(i)) {
      va_fd_mark[i] = ftell(va_fd[i]);
    }
  }
}

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
      va_fd_mark[i] = 0;
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
      /* No handle, no simulator to say it to: see `va_strobe_write`. */
      if (handle != NULL) {
        osdi_log(handle, msg, lvl);
      }
    } else if (i < VA_NFD && va_fd[i] != NULL) {
      fputs(msg, va_fd[i]);
    }
    return;
  }
  /* Channel 0 is the simulator's own output. */
  if ((d & 1) && handle != NULL) {
    osdi_log(handle, msg, lvl);
  }
  for (uint32_t b = 1; b < VA_NMCD; b++) {
    if (((d >> b) & 1) && va_mcd[b] != NULL) {
      fputs(msg, va_mcd[b]);
    }
  }
}

/* 9.4.1: "$strobe displays the simulation data at the end of the current
 * simulation time", which an analog block cannot reach by itself -- it is reached
 * many times while the solver settles one point, and nothing in it knows which
 * evaluation was the last.
 *
 * So the line comes here, and what happens to it depends on whether anything can
 * tell us that a point is final:
 *
 *   a simulator that calls `accept`   the line is held until the point is accepted
 *                                     and then written, so it is the line of the
 *                                     evaluation that converged, once per accepted
 *                                     point, and nothing at all for a point the
 *                                     solver went back on.
 *   one that does not                 the line is written as soon as it is composed
 *                                     at a new time and nothing is held, which is
 *                                     what this task did before the callback
 *                                     existed: one line per attempted point, from
 *                                     its first evaluation.
 *
 * The first point of a run is the one exception on an accepting simulator: its
 * line is composed before the first `accept` arrives, so it is written as the
 * second column above describes. Nothing can be done about that from here, and
 * nothing is lost by it.
 *
 * One pending line per instance and statement, because a statement writes one line
 * per timepoint and two instances of a model do not share theirs. The table is
 * searched linearly: it holds one entry per strobe statement per instance, which
 * is a handful in every model that logs anything.
 */
int32_t va_fflush(int32_t desc, int32_t all);

typedef struct VaStrobe {
  /* The instance the line belongs to, or NULL for a free slot. */
  void *inst;
  /* Which statement composed it, so that two in one module do not overwrite each
   * other. The lowering numbers them. */
  int32_t site;
  int32_t desc;
  uint32_t lvl;
  /* The `$abstime` it was composed at, which is how a new point is recognized. */
  double at;
  /* Owned here, and freed when written or replaced. */
  char *line;
} VaStrobe;

VA_SHARED VaStrobe *va_strobes;
VA_SHARED uint32_t va_nstrobes;
/* Set by the first `accept` call. Until one arrives the simulator is assumed never
 * to make one, and a line is written as soon as it is composed at a new time,
 * which is what this task did before the callback existed. The read positions
 * above are kept the same way, and share the flag. */
VA_SHARED bool va_accept_seen;

/* `handle` is the simulator's, and is only valid for the duration of the call it
 * came with: ngspice builds one on the stack per evaluation. So it is passed in
 * rather than kept, and a flush that has none (the library going away, `$fflush`)
 * writes to files only -- `va_fputs` leaves the simulator's own log alone when it
 * has no handle to write through. */
static void va_strobe_write(VaStrobe *slot, void *handle) {
  if (slot->line != NULL) {
    va_fputs(handle, slot->desc, slot->line, slot->lvl);
    /* The point is final, so the line is too: a measurement log is read while the
     * simulation is still going, and a line held back in a buffer is a line that
     * is not there yet. Once per accepted point, which is what the clause's own
     * "end of the current simulation time" costs. */
    va_fflush(slot->desc, 0);
    free(slot->line);
    slot->line = NULL;
  }
  slot->inst = NULL;
}

/* The pending line of this instance and statement, or a free slot for it. */
static VaStrobe *va_strobe_slot(void *inst, int32_t site) {
  VaStrobe *slot;
  uint32_t i;
  for (i = 0; i < va_nstrobes; i++) {
    if (va_strobes[i].inst == inst && va_strobes[i].site == site) {
      return &va_strobes[i];
    }
  }
  for (i = 0; i < va_nstrobes; i++) {
    if (va_strobes[i].inst == NULL) {
      return &va_strobes[i];
    }
  }
  slot = realloc(va_strobes, (va_nstrobes + 1) * sizeof(VaStrobe));
  if (slot == NULL) {
    return NULL;
  }
  va_strobes = slot;
  slot = &va_strobes[va_nstrobes];
  va_nstrobes += 1;
  slot->inst = NULL;
  slot->line = NULL;
  return slot;
}

/* `$strobe` and `$fstrobe`: keep this line as the one for `at`, and take the
 * ownership of it that the formatting handed over.
 *
 * `inst` is NULL where there is no instance to key on (a model's own setup, or
 * verilogae), and then there is no timepoint either, so the line is written as it
 * stands.
 */
void va_strobe(void *handle, void *inst, uint32_t lvl, int32_t site, int32_t desc,
               char *line, double at) {
  VaStrobe *slot;
  if (line == NULL) {
    return;
  }
  if (inst == NULL) {
    va_fputs(handle, desc, line, lvl);
    free(line);
    return;
  }
  slot = va_strobe_slot(inst, site);
  if (slot == NULL) {
    va_fputs(handle, desc, line, lvl);
    free(line);
    return;
  }

  if (!va_accept_seen) {
    /* No simulator has said that a point was accepted, so this one is assumed not
     * to: write the first evaluation at each new time and keep nothing. That is
     * what this task did before there was an accept callback, down to the line it
     * picks, so a simulator that does not call one sees no change at all. The slot
     * is kept for the time, which is all that is needed to recognize a new point.
     */
    bool is_new = slot->inst != inst || slot->at != at;
    slot->inst = inst;
    slot->site = site;
    slot->at = at;
    if (is_new) {
      va_fputs(handle, desc, line, lvl);
    }
    free(line);
    return;
  }

  /* A simulator that does confirm its points: hold the line, replacing whatever
   * an earlier evaluation of this point left, and let `accept` decide that it was
   * the last word. A point the solver goes back on is simply overwritten. */
  if (slot->inst == inst && slot->line != NULL) {
    free(slot->line);
  }
  slot->inst = inst;
  slot->site = site;
  slot->desc = desc;
  slot->lvl = lvl;
  slot->at = at;
  slot->line = line;
}

/* The descriptor's `accept`: the timepoint just evaluated is final. So every line
 * this instance composed during it is the line for it, and whatever it read is
 * read: a point the solver goes back on writes nothing and consumes nothing. */
void va_accept(void *handle, void *inst, void *model, void *sim_info) {
  uint32_t i;
  (void)model;
  (void)sim_info;
  va_accept_seen = 1;
  for (i = 0; i < va_nstrobes; i++) {
    if (va_strobes[i].inst == inst) {
      va_strobe_write(&va_strobes[i], handle);
    }
  }
  va_read_commit();
}

/* Nothing else holds the last line of a run: no further point arrives to push it
 * out, and a simulator that accepts points has already had the last one. */
static void va_strobe_flush(void) {
  uint32_t i;
  for (i = 0; i < va_nstrobes; i++) {
    va_strobe_write(&va_strobes[i], NULL);
  }
}

__attribute__((destructor)) static void va_strobe_atexit(void) { va_strobe_flush(); }

/* 9.5.6: "$fflush(mcd) ... writes any buffered output"; with no argument it
 * flushes everything this library has open. */
int32_t va_fflush(int32_t desc, int32_t all) {
  /* A line this library is holding is buffered output too. */
  if (all) {
    va_strobe_flush();
  }
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

/* 9.5.4: "$fgets reads characters from the file ... until a newline is read and
 * transferred ... or an EOF condition is encountered", and returns the number of
 * characters read.
 *
 * The line comes back as the return value rather than through the argument,
 * because a Verilog-A argument is not a place a runtime can write to; the
 * lowering assigns it, and asks for the length separately. The buffer is exactly
 * as long as the line, and nothing frees it -- a retained string variable holds
 * this pointer across a timestep, and so may the next one.
 */
char *va_fgets(int32_t desc) {
  /* IEEE 1364 does not bound a line, so this does. */
  char line[4096];
  void *f = va_one_file(desc);
  size_t len;
  char *out;
  if (f == NULL) {
    return "";
  }
  if (fgets(line, (int)sizeof(line), f) == NULL) {
    return "";
  }
  len = strlen(line);
  out = malloc(len + 1);
  if (out == NULL) {
    return "";
  }
  memcpy(out, line, len + 1);
  return out;
}

/* How much of it there was, which is what the task returns. Separate so that the
 * read above stays a single call: asking twice would read twice. */
int32_t va_strlen(const char *s) { return s == NULL ? 0 : (int32_t)strlen(s); }

/* 9.5.4: `$sscanf` and `$fscanf`.
 *
 * The conversions are performed here rather than by handing the arguments to
 * libc's `sscanf`, because what a Verilog-A model passes are variables to be
 * assigned, not pointers to write through -- there is nothing to hand over. So
 * each conversion lands in a table that the lowering reads back one entry at a
 * time, and the count of them comes back as the task's own result.
 *
 * What is converted is what a data file is made of: runs of whitespace, literal
 * characters, `%%`, and `%d`, `%e`/`%f`/`%g`/`%r` and `%s`, with a field width
 * accepted and ignored. Anything else stops the scan, which is what the clause
 * says a failing conversion does.
 */
#define VA_SCAN_MAX 16
static double va_scan_nums[VA_SCAN_MAX];
static char *va_scan_strs[VA_SCAN_MAX];

static int va_isspace(char c) {
  return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v';
}

int32_t va_sscanf(const char *str, const char *fmt) {
  const char *s = str;
  const char *f = fmt;
  int n = 0;
  if (str == NULL || fmt == NULL) {
    return -1;
  }
  while (*f != '\0') {
    if (va_isspace(*f)) {
      while (va_isspace(*s)) {
        s++;
      }
      f++;
      continue;
    }
    if (*f != '%') {
      if (*s != *f) {
        break;
      }
      s++;
      f++;
      continue;
    }
    f++;
    if (*f == '%') {
      if (*s != '%') {
        break;
      }
      s++;
      f++;
      continue;
    }
    /* A field width, which the conversions below do not need. */
    while (*f >= '0' && *f <= '9') {
      f++;
    }
    while (va_isspace(*s)) {
      s++;
    }
    if (*s == '\0' || n >= VA_SCAN_MAX) {
      break;
    }
    if (*f == 'd' || *f == 'D') {
      char *end;
      long v = strtol(s, &end, 10);
      if (end == s) {
        break;
      }
      va_scan_nums[n] = (double)v;
      s = end;
    } else if (*f == 'e' || *f == 'E' || *f == 'f' || *f == 'F' || *f == 'g' ||
               *f == 'G' || *f == 'r' || *f == 'R') {
      char *end;
      double v = strtod(s, &end);
      if (end == s) {
        break;
      }
      va_scan_nums[n] = v;
      s = end;
    } else if (*f == 's' || *f == 'S') {
      const char *start = s;
      size_t len;
      char *out;
      while (*s != '\0' && !va_isspace(*s)) {
        s++;
      }
      len = (size_t)(s - start);
      out = malloc(len + 1);
      if (out == NULL) {
        break;
      }
      memcpy(out, start, len);
      out[len] = '\0';
      va_scan_strs[n] = out;
      va_scan_nums[n] = 0.0;
    } else {
      break;
    }
    n++;
    f++;
  }
  /* "returns EOF if the end of the file is reached": nothing converted and
   * nothing left to convert. */
  if (n == 0 && *s == '\0') {
    return -1;
  }
  return (int32_t)n;
}

double va_scan_real(int32_t k) {
  return (k >= 0 && k < VA_SCAN_MAX) ? va_scan_nums[k] : 0.0;
}

char *va_scan_str(int32_t k) {
  char *s = (k >= 0 && k < VA_SCAN_MAX) ? va_scan_strs[k] : NULL;
  return s == NULL ? "" : s;
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
 * `commit_retained` runs once at the top of every eval. When the simulator makes
 * the `accept` call of the timepoint protocol, that call is what promotes them,
 * and this only records the time. Otherwise it uses `$abstime` to decide what
 * happened since the last call:
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

/* A string variable that has to survive a timestep.
 *
 * 4.5.10 makes no exception for strings: a variable keeps what it held. A
 * retained slot is eight bytes of instance data, and what goes in it for a
 * string is the pointer, because that is what a string value is here -- either
 * into the library's own constants, where it is valid for as long as the library
 * is loaded, or onto the heap, where nothing in this runtime frees it.
 *
 * The slot starts as zero, which reads back as the empty string: the value a
 * string variable has before anything assigns one.
 */
double store_retained_str(double *dst, char *val) {
  memcpy(dst, &val, sizeof(char *));
  return 0.0;
}

char *retained_prev_str(double *src) {
  char *val;
  memcpy(&val, src, sizeof(char *));
  return val == NULL ? "" : val;
}

/* Promote every slot: what this timestep wrote is what the next one reads back.
 *
 * Copied as bytes rather than as a number, because a slot holding a string holds
 * a pointer, and a pointer is not something arithmetic should be given the chance
 * to touch. */
void commit_retained_now(double *vals, uint32_t num) {
  uint32_t i;
  for (i = 0; i < num; i++) {
    memcpy(&vals[2 * i], &vals[2 * i + 1], sizeof(double));
  }
}

void commit_retained(double *vals, double *time, uint32_t num, double abstime) {
  /* Once a simulator has said that a point was accepted, it says so for every
   * accepted point, and the generated `accept` promotes these slots there. Going
   * on `$abstime` as well would promote at a point that has not been accepted
   * yet, which is the same mistake the read positions would make: a model's
   * retained state and its file position are one state. */
  if (!va_accept_seen && abstime > *time) {
    commit_retained_now(vals, num);
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
