#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <inttypes.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <pwd.h>
#include <sys/types.h>
#include <unistd.h>

#include <ngspice/sharedspice.h>

typedef struct {
    char **input_sources;
    double *inputs;
    size_t input_count;
    char **output_vectors;
    size_t output_count;
    volatile bool background_running;
    volatile bool controlled_exit;
    volatile int controlled_exit_status;
} context_t;

static bool diagnostics_enabled(void) {
    const char *value = getenv("ONTOLOGYX_SIM_NGSPICE_DIAGNOSTICS");
    return value != NULL && value[0] != '\0' && strcmp(value, "0") != 0;
}

static void diagnostic_stage(const char *stage) {
    if(!diagnostics_enabled()) return;
    fprintf(stderr, "ontologyx-ngspice-cosim-helper: stage=%s\n", stage);
    fflush(stderr);
}

static bool readable_file(const char *path) {
    return path != NULL && path[0] != '\0' && access(path, R_OK) == 0;
}

static void reject_ambient_user_spiceinit(void) {
    const char *names[] = {".spiceinit", "spice.rc"};
    char path[4096];

    for(size_t i = 0; i < sizeof(names) / sizeof(names[0]); ++i) {
        if(readable_file(names[i])) {
            fprintf(stderr,
                    "ontologyx-ngspice-cosim-helper: refusing ambient ngspice user config `%s`; "
                    "run the participant from an isolated directory\n",
                    names[i]);
            exit(2);
        }
    }

    const char *home = getenv("HOME");
    if(home != NULL && home[0] != '\0') {
        for(size_t i = 0; i < sizeof(names) / sizeof(names[0]); ++i) {
            int written = snprintf(path, sizeof(path), "%s/%s", home, names[i]);
            if(written > 0 && (size_t)written < sizeof(path) && readable_file(path)) {
                fprintf(stderr,
                        "ontologyx-ngspice-cosim-helper: refusing ambient ngspice user config `%s`\n",
                        path);
                exit(2);
            }
        }
    }

    struct passwd *password = getpwuid(getuid());
    if(password != NULL && password->pw_dir != NULL && password->pw_dir[0] != '\0') {
        for(size_t i = 0; i < sizeof(names) / sizeof(names[0]); ++i) {
            int written = snprintf(path, sizeof(path), "%s/%s", password->pw_dir, names[i]);
            if(written > 0 && (size_t)written < sizeof(path) && readable_file(path)) {
                fprintf(stderr,
                        "ontologyx-ngspice-cosim-helper: refusing ambient ngspice user config `%s`; "
                        "SharedSpice initialization must not depend on per-user startup commands\n",
                        path);
                exit(2);
            }
        }
    }
}

static void die(const char *message) {
    fprintf(stderr, "ontologyx-ngspice-cosim-helper: %s\n", message);
    fflush(stderr);
    exit(2);
}

static char *lower_copy(const char *value) {
    size_t length = strlen(value);
    char *copy = malloc(length + 1);
    if(copy == NULL) die("out of memory");
    for(size_t i = 0; i < length; ++i) {
        char c = value[i];
        copy[i] = (c >= 'A' && c <= 'Z') ? (char)(c - 'A' + 'a') : c;
    }
    copy[length] = '\0';
    return copy;
}

static uint64_t parse_u64(const char *text, const char *what) {
    errno = 0;
    char *end = NULL;
    unsigned long long value = strtoull(text, &end, 10);
    if(errno != 0 || end == text || *end != '\0') {
        fprintf(stderr, "ontologyx-ngspice-cosim-helper: invalid %s `%s`\n", what, text);
        exit(2);
    }
    return (uint64_t)value;
}

static int send_char(char *message, int id, void *user_data) {
    (void)id; (void)user_data;
    if(message != NULL && (strstr(message, "stderr") == message || strstr(message, "Error") != NULL || strstr(message, "ERROR") != NULL)) {
        fprintf(stderr, "ngspice: %s\n", message);
    }
    return 0;
}

static int controlled_exit(int status, bool immediate, bool quit, int id, void *user_data) {
    (void)immediate; (void)quit; (void)id;
    context_t *context = user_data;
    context->controlled_exit = true;
    context->controlled_exit_status = status;
    return 0;
}

static int bg_running(bool running, int id, void *user_data) {
    (void)id;
    context_t *context = user_data;
    context->background_running = running;
    return 0;
}

static int get_vsrc(double *voltage, double actual_time, char *name, int id, void *user_data) {
    (void)actual_time; (void)id;
    context_t *context = user_data;
    if(voltage == NULL || name == NULL) return 1;
    *voltage = 0.0;
    char *lower = lower_copy(name);
    int rc = 1;
    for(size_t i = 0; i < context->input_count; ++i) {
        if(strcmp(lower, context->input_sources[i]) == 0) {
            *voltage = context->inputs[i];
            rc = 0;
            break;
        }
    }
    free(lower);
    return rc;
}

static int get_sync(double actual_time, double *delta, double old_delta, int redostep, int id, int location, void *user_data) {
    (void)actual_time; (void)delta; (void)old_delta; (void)redostep; (void)id; (void)location; (void)user_data;
    return 0;
}

static char **read_netlist(const char *path) {
    FILE *file = fopen(path, "r");
    if(file == NULL) die("could not open netlist");
    char **lines = NULL;
    size_t count = 0;
    char *line = NULL;
    size_t capacity = 0;
    while(getline(&line, &capacity, file) >= 0) {
        char *newline = strpbrk(line, "\r\n");
        if(newline != NULL) *newline = '\0';
        char **next = realloc(lines, (count + 2) * sizeof(*lines));
        if(next == NULL) die("out of memory");
        lines = next;
        lines[count] = strdup(line);
        if(lines[count] == NULL) die("out of memory");
        ++count;
        lines[count] = NULL;
    }
    free(line);
    fclose(file);
    if(lines == NULL) die("netlist is empty");
    return lines;
}

static void free_netlist(char **lines) {
    if(lines == NULL) return;
    for(size_t i = 0; lines[i] != NULL; ++i) free(lines[i]);
    free(lines);
}

static double last_real(const char *name) {
    pvector_info info = ngGet_Vec_Info((char *)name);
    if(info == NULL || info->v_length <= 0 || info->v_realdata == NULL) {
        fprintf(stderr, "ontologyx-ngspice-cosim-helper: vector `%s` is unavailable\n", name);
        exit(4);
    }
    double value = info->v_realdata[info->v_length - 1];
    if(!isfinite(value)) die("ngspice returned a non-finite vector value");
    return value;
}

static uint64_t actual_time_ps(void) {
    double seconds = last_real("time");
    if(seconds < 0.0 || seconds > (double)UINT64_MAX / 1.0e12) die("ngspice time is outside picosecond range");
    return (uint64_t)llround(seconds * 1.0e12);
}

static void write_state(context_t *context, uint64_t protocol_time_ps) {
    printf("STATE %" PRIu64, protocol_time_ps);
    for(size_t i = 0; i < context->output_count; ++i) {
        printf(" %.17e", last_real(context->output_vectors[i]));
    }
    putchar('\n');
    fflush(stdout);
}

static void command_checked(const char *command) {
    int rc = ngSpice_Command((char *)command);
    if(rc != 0) {
        fprintf(stderr, "ontologyx-ngspice-cosim-helper: ngSpice_Command failed (%d): %s\n", rc, command);
        exit(4);
    }
}

static void wait_background(context_t *context) {
    const struct timespec sleep_time = {.tv_sec = 0, .tv_nsec = 1000000L};
    uint64_t polls = 0;
    while(ngSpice_running() || context->background_running) {
        if(context->controlled_exit) {
            fprintf(stderr, "ontologyx-ngspice-cosim-helper: ngspice requested exit status %d\n", context->controlled_exit_status);
            exit(4);
        }
        nanosleep(&sleep_time, NULL);
        if(++polls > 600000ULL) die("timed out waiting for ngspice background simulation");
    }
    if(context->controlled_exit) {
        fprintf(stderr, "ontologyx-ngspice-cosim-helper: ngspice requested exit status %d\n", context->controlled_exit_status);
        exit(4);
    }
}

static void advance_to(context_t *context, uint64_t target_ps, bool *started) {
    double target_seconds = (double)target_ps / 1.0e12;
    char stop[128];
    snprintf(stop, sizeof(stop), "stop when time = %.17e", target_seconds);
    command_checked(stop);
    context->background_running = true;
    command_checked(*started ? "bg_resume" : "bg_run");
    *started = true;
    wait_background(context);
    uint64_t actual = actual_time_ps();
    uint64_t difference = actual > target_ps ? actual - target_ps : target_ps - actual;
    if(difference > 2ULL) {
        fprintf(stderr,
                "ontologyx-ngspice-cosim-helper: transient stopped at %" PRIu64 " ps instead of %" PRIu64 " ps\n",
                actual, target_ps);
        exit(4);
    }
    write_state(context, target_ps);
}

int main(int argc, char **argv) {
    const char *netlist_path = NULL;
    context_t context = {0};

    for(int i = 1; i < argc; ++i) {
        if(strcmp(argv[i], "--netlist") == 0 && i + 1 < argc) {
            netlist_path = argv[++i];
        } else if(strcmp(argv[i], "--input-source") == 0 && i + 1 < argc) {
            char **next_names = realloc(context.input_sources, (context.input_count + 1) * sizeof(*context.input_sources));
            double *next_values = realloc(context.inputs, (context.input_count + 1) * sizeof(*context.inputs));
            if(next_names == NULL || next_values == NULL) die("out of memory");
            context.input_sources = next_names;
            context.inputs = next_values;
            context.input_sources[context.input_count] = lower_copy(argv[++i]);
            context.inputs[context.input_count] = 0.0;
            ++context.input_count;
        } else if(strcmp(argv[i], "--output-vector") == 0 && i + 1 < argc) {
            char **next = realloc(context.output_vectors, (context.output_count + 1) * sizeof(*context.output_vectors));
            if(next == NULL) die("out of memory");
            context.output_vectors = next;
            context.output_vectors[context.output_count++] = strdup(argv[++i]);
        } else {
            die("usage: --netlist PATH [--input-source NAME]... --output-vector VECTOR ...");
        }
    }
    if(netlist_path == NULL || context.output_count == 0) die("netlist and at least one output vector are required");

    /*
     * Do not call ngSpice_nospiceinit() here.  Some packaged SharedSpice
     * builds implement that pre-init API through frontend variable state that
     * is not initialized until ngSpice_Init(), which can terminate the helper
     * with SIGSEGV before the normal callback boundary exists.  The helper is
     * already launched in an isolated cwd/HOME by the Rust participant; the
     * explicit guard below additionally refuses a per-user startup file that a
     * platform build might discover through passwd lookup.
     */
    reject_ambient_user_spiceinit();
    diagnostic_stage("before-ngSpice_Init");
    if(ngSpice_Init(send_char, NULL, controlled_exit, NULL, NULL, bg_running, &context) != 0) {
        die("ngSpice_Init failed");
    }
    diagnostic_stage("after-ngSpice_Init");

    int ident = 0;
    if(ngSpice_Init_Sync(get_vsrc, NULL, get_sync, &ident, &context) != 0) {
        die("ngSpice_Init_Sync failed");
    }
    diagnostic_stage("after-ngSpice_Init_Sync");

    char **netlist = read_netlist(netlist_path);
    if(ngSpice_Circ(netlist) != 0) die("ngSpice_Circ failed");
    free_netlist(netlist);
    diagnostic_stage("after-ngSpice_Circ");

    command_checked("set noaskquit");
    command_checked("option numdgt=17");
    diagnostic_stage("before-op");
    command_checked("op");
    diagnostic_stage("after-op");
    printf("STATE 0");
    for(size_t i = 0; i < context.output_count; ++i) {
        printf(" %.17e", last_real(context.output_vectors[i]));
    }
    putchar('\n');
    fflush(stdout);

    bool started = false;
    uint64_t current_ps = 0;
    char *line = NULL;
    size_t capacity = 0;
    while(getline(&line, &capacity, stdin) >= 0) {
        char *newline = strpbrk(line, "\r\n");
        if(newline != NULL) *newline = '\0';
        if(strcmp(line, "QUIT") == 0) break;
        if(strncmp(line, "SET ", 4) == 0) {
            char *save = NULL;
            char *index_text = strtok_r(line + 4, " ", &save);
            char *value_text = strtok_r(NULL, " ", &save);
            char *extra = strtok_r(NULL, " ", &save);
            if(index_text == NULL || value_text == NULL || extra != NULL) die("invalid SET command");
            uint64_t index = parse_u64(index_text, "input index");
            if(index >= context.input_count) die("SET input index out of range");
            errno = 0;
            char *end = NULL;
            double value = strtod(value_text, &end);
            if(errno != 0 || end == value_text || *end != '\0' || !isfinite(value)) die("SET value must be finite");
            context.inputs[index] = value;
            if(!started && current_ps == 0) {
                command_checked("op");
            }
            write_state(&context, current_ps);
            continue;
        }
        if(strncmp(line, "ADV ", 4) == 0) {
            uint64_t target_ps = parse_u64(line + 4, "target time");
            if(target_ps < current_ps) die("ngspice cannot move backwards in time");
            if(target_ps == current_ps) {
                write_state(&context, current_ps);
                continue;
            }
            advance_to(&context, target_ps, &started);
            current_ps = target_ps;
            continue;
        }
        die("unknown protocol command");
    }

    free(line);
    if(started && ngSpice_running()) command_checked("bg_halt");
    command_checked("quit");
    for(size_t i = 0; i < context.input_count; ++i) free(context.input_sources[i]);
    for(size_t i = 0; i < context.output_count; ++i) free(context.output_vectors[i]);
    free(context.input_sources);
    free(context.inputs);
    free(context.output_vectors);
    return 0;
}
