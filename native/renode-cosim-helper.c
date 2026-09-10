#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <inttypes.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "renode_api.h"

typedef struct {
    char *controller_name;
    int32_t pin;
    char direction[3];
    renode_gpio_t *gpio;
} binding_t;

static void die(const char *message) {
    fprintf(stderr, "ontologyx-renode-cosim-helper: %s\n", message);
    fflush(stderr);
    exit(2);
}

static void check_renode(renode_error_t *error, const char *operation) {
    if(error == NULL) {
        return;
    }
    fprintf(stderr, "ontologyx-renode-cosim-helper: Renode API failed during %s\n", operation);
    renode_free_error(error);
    fflush(stderr);
    exit(3);
}

static void fatal_error(void *user_data, renode_error_t *error) {
    (void)user_data;
    fprintf(stderr, "ontologyx-renode-cosim-helper: Renode External Control fatal error\n");
    if(error != NULL && error->message != NULL) {
        fprintf(stderr, "ontologyx-renode-cosim-helper: %s\n", error->message);
    }
    fflush(stderr);
}

static uint64_t parse_u64(const char *text, const char *what) {
    errno = 0;
    char *end = NULL;
    unsigned long long value = strtoull(text, &end, 10);
    if(errno != 0 || end == text || *end != '\0') {
        fprintf(stderr, "ontologyx-renode-cosim-helper: invalid %s `%s`\n", what, text);
        exit(2);
    }
    return (uint64_t)value;
}

static int32_t parse_i32(const char *text, const char *what) {
    errno = 0;
    char *end = NULL;
    long value = strtol(text, &end, 10);
    if(errno != 0 || end == text || *end != '\0' || value < 0 || value > INT32_MAX) {
        fprintf(stderr, "ontologyx-renode-cosim-helper: invalid %s `%s`\n", what, text);
        exit(2);
    }
    return (int32_t)value;
}

static binding_t parse_binding(const char *value) {
    char *copy = strdup(value);
    if(copy == NULL) die("out of memory");
    char *save = NULL;
    char *controller = strtok_r(copy, ",", &save);
    char *pin = strtok_r(NULL, ",", &save);
    char *direction = strtok_r(NULL, ",", &save);
    char *extra = strtok_r(NULL, ",", &save);
    if(controller == NULL || pin == NULL || direction == NULL || extra != NULL) {
        free(copy);
        die("--binding must be CONTROLLER,PIN,in|out|io");
    }
    if(strcmp(direction, "in") != 0 && strcmp(direction, "out") != 0 && strcmp(direction, "io") != 0) {
        free(copy);
        die("binding direction must be in, out, or io");
    }
    binding_t binding = {0};
    binding.controller_name = strdup(controller);
    if(binding.controller_name == NULL) die("out of memory");
    binding.pin = parse_i32(pin, "GPIO pin");
    snprintf(binding.direction, sizeof(binding.direction), "%s", direction);
    free(copy);
    return binding;
}

static uint64_t current_time_ps(renode_t *renode) {
    renode_time_t time = 0;
    check_renode(renode_get_current_time(renode, &time), "get_current_time");
    uint64_t ns = renode_time_to_time_unit(time, TU_NANOSECONDS);
    if(ns > UINT64_MAX / 1000ULL) die("Renode virtual time exceeds picosecond range");
    return ns * 1000ULL;
}

static void write_state(renode_t *renode, binding_t *bindings, size_t count) {
    uint64_t time_ps = current_time_ps(renode);
    printf("STATE %" PRIu64, time_ps);
    for(size_t i = 0; i < count; ++i) {
        bool state = false;
        check_renode(renode_get_gpio_state(bindings[i].gpio, bindings[i].pin, &state), "get_gpio_state");
        printf(" %d", state ? 1 : 0);
    }
    putchar('\n');
    fflush(stdout);
}

int main(int argc, char **argv) {
    const char *server_port = NULL;
    const char *machine_name = NULL;
    binding_t *bindings = NULL;
    size_t binding_count = 0;

    for(int i = 1; i < argc; ++i) {
        if(strcmp(argv[i], "--server-port") == 0 && i + 1 < argc) {
            server_port = argv[++i];
        } else if(strcmp(argv[i], "--machine") == 0 && i + 1 < argc) {
            machine_name = argv[++i];
        } else if(strcmp(argv[i], "--binding") == 0 && i + 1 < argc) {
            binding_t *next = realloc(bindings, (binding_count + 1) * sizeof(*bindings));
            if(next == NULL) die("out of memory");
            bindings = next;
            bindings[binding_count++] = parse_binding(argv[++i]);
        } else {
            die("usage: --server-port PORT --machine NAME --binding CONTROLLER,PIN,in|out|io ...");
        }
    }
    if(server_port == NULL || machine_name == NULL || binding_count == 0) {
        die("server port, machine, and at least one binding are required");
    }

    renode_t *renode = NULL;
    check_renode(renode_connect(server_port, &renode), "connect");
    renode_set_fatal_error_callback(renode, NULL, fatal_error);
    renode_machine_t *machine = NULL;
    check_renode(renode_get_machine(renode, machine_name, &machine), "get_machine");
    for(size_t i = 0; i < binding_count; ++i) {
        check_renode(renode_get_gpio(machine, bindings[i].controller_name, &bindings[i].gpio), "get_gpio");
    }

    write_state(renode, bindings, binding_count);
    char *line = NULL;
    size_t capacity = 0;
    while(getline(&line, &capacity, stdin) >= 0) {
        char *newline = strpbrk(line, "\r\n");
        if(newline != NULL) *newline = '\0';
        if(strcmp(line, "QUIT") == 0) {
            break;
        }
        if(strncmp(line, "SET ", 4) == 0) {
            char *save = NULL;
            char *index_text = strtok_r(line + 4, " ", &save);
            char *state_text = strtok_r(NULL, " ", &save);
            char *extra = strtok_r(NULL, " ", &save);
            if(index_text == NULL || state_text == NULL || extra != NULL) die("invalid SET command");
            uint64_t index = parse_u64(index_text, "binding index");
            if(index >= binding_count) die("SET binding index out of range");
            if(strcmp(bindings[index].direction, "out") == 0) die("cannot SET an output-only GPIO binding");
            if(strcmp(state_text, "0") != 0 && strcmp(state_text, "1") != 0) die("SET state must be 0 or 1");
            check_renode(
                renode_set_gpio_state(bindings[index].gpio, bindings[index].pin, strcmp(state_text, "1") == 0),
                "set_gpio_state"
            );
            write_state(renode, bindings, binding_count);
            continue;
        }
        if(strncmp(line, "ADV ", 4) == 0) {
            uint64_t target_ps = parse_u64(line + 4, "target time");
            if(target_ps % 1000ULL != 0) die("Renode target time must be a whole number of nanoseconds");
            uint64_t now_ps = current_time_ps(renode);
            if(target_ps < now_ps) die("Renode cannot move backwards in virtual time");
            uint64_t delta_ns = (target_ps - now_ps) / 1000ULL;
            if(delta_ns > 0) {
                renode_time_t delta = 0;
                check_renode(renode_create_time(delta_ns, TU_NANOSECONDS, &delta), "create_time");
                check_renode(renode_run_for(renode, delta), "run_for");
            }
            if(current_time_ps(renode) != target_ps) die("Renode did not reach the requested virtual time exactly");
            write_state(renode, bindings, binding_count);
            continue;
        }
        die("unknown protocol command");
    }

    free(line);
    for(size_t i = 0; i < binding_count; ++i) {
        free(bindings[i].gpio);
        free(bindings[i].controller_name);
    }
    free(bindings);
    free(machine);
    check_renode(renode_disconnect(&renode), "disconnect");
    return 0;
}
