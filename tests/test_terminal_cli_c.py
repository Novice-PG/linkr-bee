"""Tests for the Linux C host CLI (tools/linkr_ble_terminal.c).

Run with: python3 -m unittest discover -s tests -v

Two layers:

* The harness compiles the real pure helpers straight out of the C source, so
  argument validation, name prefixing, escape naming and Enter translation are
  asserted without a Bluetooth stack, a D-Bus daemon or even the dbus headers.
  It runs everywhere, including CI runners without libdbus-1-dev. usage_fatal()
  is stubbed with a longjmp so the failure paths can be checked in-process.
* The binary layer builds the whole client with -Werror and drives the argument
  surface that exits before D-Bus is touched. It skips when dbus-1 is missing.

The D-Bus, GATT and terminal paths themselves need a BlueZ adapter and a real
Bee, and are not covered here.
"""

import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SOURCE_PATH = ROOT / "tools/linkr_ble_terminal.c"
PYTHON_PATH = ROOT / "tools/linkr_ble_terminal.py"
CC = os.environ.get("CC", "cc")


def extract(source, start_marker, end_marker, required):
    """Slice a production block by literal anchors.

    Copied from tests/test_regressions.py: a moved anchor must fail loudly
    instead of quietly testing a different function.
    """
    start = source.index(start_marker)
    block = source[start:source.index(end_marker, start)]
    for name in required:
        if f"{name}(" not in block:
            raise AssertionError(f"{name} is missing from the extracted block")
    return block


def c_version_string():
    match = re.search(r'#define CLI_VERSION\s+"([^"]+)"', SOURCE_PATH.read_text())
    return match.group(1) if match else ""


def dbus_available():
    if not shutil.which(CC) or not shutil.which("pkg-config"):
        return False
    return subprocess.run(["pkg-config", "--exists", "dbus-1"],
                          capture_output=True).returncode == 0


HARNESS_PRELUDE = r"""
#include <ctype.h>
#include <errno.h>
#include <math.h>
#include <setjmp.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_NAME_LEN_LOCAL 128
#define BLE_MAX_NUS_PAYLOAD 244

/* The extracted validators report through usage_fatal(); catch it here so the
 * failure paths can be asserted without exiting the harness. */
static jmp_buf escape_jump;
static char last_usage_error[256];

static void usage_fatal(const char *fmt, ...)
{
    va_list ap;

    va_start(ap, fmt);
    vsnprintf(last_usage_error, sizeof(last_usage_error), fmt, ap);
    va_end(ap);
    longjmp(escape_jump, 1);
}
"""

HARNESS_MAIN = r"""
static int failures;

#define CHECK(cond)                                                          \
    do {                                                                     \
        if (!(cond)) {                                                       \
            printf("FAIL line %d: %s\n", __LINE__, #cond);                   \
            failures++;                                                      \
        }                                                                    \
    } while (0)

#define CHECK_STR(got, want)                                                 \
    do {                                                                     \
        if (strcmp((got), (want)) != 0) {                                    \
            printf("FAIL line %d: got \"%s\", want \"%s\"\n",                \
                   __LINE__, (got), (want));                                 \
            failures++;                                                      \
        }                                                                    \
    } while (0)

/* Run parse_escape and report whether it was rejected, plus the message. */
static bool escape_rejected(const char *value)
{
    if (setjmp(escape_jump) == 0) {
        parse_escape(value);
        return false;
    }
    return true;
}

static bool double_rejected(const char *value)
{
    if (setjmp(escape_jump) == 0) {
        parse_positive_double("--timeout", value);
        return false;
    }
    return true;
}

static bool write_size_rejected(const char *value)
{
    if (setjmp(escape_jump) == 0) {
        parse_ble_write_size(value);
        return false;
    }
    return true;
}

static void check_enter(const uint8_t *in, size_t in_len, const char *mode,
                        const char *want)
{
    uint8_t out[64];
    size_t out_len = 0;

    normalize_enter(in, in_len, mode, out, &out_len, sizeof(out));
    out[out_len] = '\0';
    if (strcmp((const char *)out, want) != 0) {
        printf("FAIL normalize_enter(%s): got \"%s\", want \"%s\"\n",
               mode, (const char *)out, want);
        failures++;
    }
}

int main(void)
{
    char buf[MAX_NAME_LEN_LOCAL];
    char small[4];
    char escape[16];

    /* normalize_name_prefix trims, drops a trailing '*' and truncates. */
    normalize_name_prefix("  Linkr BLE UART*  ", buf, sizeof(buf));
    CHECK_STR(buf, "Linkr BLE UART");
    normalize_name_prefix("Linkr BLE UART", buf, sizeof(buf));
    CHECK_STR(buf, "Linkr BLE UART");
    normalize_name_prefix("*", buf, sizeof(buf));
    CHECK_STR(buf, "");
    normalize_name_prefix("", buf, sizeof(buf));
    CHECK_STR(buf, "");
    normalize_name_prefix("0123456789", small, sizeof(small));
    CHECK_STR(small, "012");
    normalize_name_prefix(NULL, buf, sizeof(buf));

    /* strip_dashes lowercases and removes separators for UUID comparison. */
    strip_dashes("6E400001-B5A3-F393-E0A9-E50E24DCCA9E", buf, sizeof(buf));
    CHECK_STR(buf, "6e400001b5a3f393e0a9e50e24dcca9e");

    /* normalize_enter matches the Python client's translate_enter. */
    check_enter((const uint8_t *)"a\r\nb\r", 6, "raw", "a\r\nb\r");
    check_enter((const uint8_t *)"a\r\nb\r", 6, "lf", "a\nb\n");
    check_enter((const uint8_t *)"a\n", 2, "cr", "a\r");
    check_enter((const uint8_t *)"a\n", 2, "crlf", "a\r\n");
    /* A CRLF pair becomes exactly one replacement, never two. */
    check_enter((const uint8_t *)"\r\n", 2, "crlf", "\r\n");

    /* describe_escape names the configured byte like the Python client. */
    describe_escape("\x1d", escape, sizeof(escape));
    CHECK_STR(escape, "Ctrl-]");
    describe_escape("\x03", escape, sizeof(escape));
    CHECK_STR(escape, "Ctrl-C");
    describe_escape("\x1b", escape, sizeof(escape));
    CHECK_STR(escape, "Esc");
    describe_escape("\x1c", escape, sizeof(escape));
    CHECK_STR(escape, "Ctrl-\\");
    describe_escape("q", escape, sizeof(escape));
    CHECK_STR(escape, "'q'");
    describe_escape("\x00", escape, sizeof(escape));
    CHECK_STR(escape, "0x00");
    describe_escape("\x80", escape, sizeof(escape));
    CHECK_STR(escape, "0x80");

    /* parse_escape: the ^X form, hex and a literal byte, plus the rejects. */
    CHECK(!escape_rejected("^]"));
    CHECK_STR(parse_escape("^]"), "\x1d");
    CHECK(!escape_rejected("0x1d"));
    CHECK_STR(parse_escape("0x1d"), "\x1d");
    CHECK(!escape_rejected("0x00"));
    CHECK_STR(parse_escape("0x00"), "");
    CHECK(!escape_rejected("q"));
    CHECK_STR(parse_escape("q"), "q");
    CHECK(escape_rejected("^"));
    CHECK_STR(last_usage_error, "escape must be one byte, like ^] or 0x1d");
    CHECK(escape_rejected("0x1ff"));
    CHECK_STR(last_usage_error, "hex escape must be between 0x00 and 0xff");
    CHECK(escape_rejected("0xzz"));
    CHECK(escape_rejected("ab"));
    CHECK(escape_rejected(""));

    /* Numeric validators. */
    CHECK(!double_rejected("2.5"));
    CHECK(double_rejected("0"));
    CHECK(double_rejected("-1"));
    CHECK(double_rejected("abc"));
    CHECK(double_rejected(""));
    CHECK(!write_size_rejected("0"));
    CHECK(!write_size_rejected("244"));
    CHECK(write_size_rejected("245"));
    CHECK(write_size_rejected("-1"));
    CHECK(write_size_rejected("x"));

    /* An exact name outranks a prefix match, and NUS support breaks ties. */
    CHECK(device_rank(true, true) > device_rank(true, false));
    CHECK(device_rank(true, false) > device_rank(false, true));
    CHECK(device_rank(false, true) > device_rank(false, false));

    if (failures) {
        printf("%d harness check(s) failed\n", failures);
        return 1;
    }
    printf("harness ok\n");
    return 0;
}
"""


class CHarnessTests(unittest.TestCase):
    """Pure C helpers, compiled from the production source with no D-Bus."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="linkr-c-harness-")
        cls.addClassCleanup(cls.tmp.cleanup)
        directory = Path(cls.tmp.name)
        source = SOURCE_PATH.read_text()

        device_helpers = extract(
            source, "static int device_rank(", "static bool iter_uuid_array_contains",
            ["device_rank", "strip_dashes", "normalize_name_prefix"])
        enter = extract(source, "static void normalize_enter(",
                        "static void describe_escape(", ["normalize_enter"])
        escape_helpers = extract(source, "static void describe_escape(", "\n/* ---",
                                 ["describe_escape"])
        validators = extract(source, "static const char *parse_escape(",
                             "static void usage(", [
                                 "parse_escape", "parse_positive_double",
                                 "parse_ble_write_size"])

        harness = directory / "harness.c"
        harness.write_text(
            HARNESS_PRELUDE + "\n"
            + escape_helpers + "\n" + enter + "\n" + device_helpers + "\n"
            + validators + "\n" + HARNESS_MAIN
        )
        cls.binary = directory / "harness"
        # -Werror: the harness also enforces that the extracted helpers are
        # warning-free, which the Makefile's plain -Wall -Wextra does not.
        result = subprocess.run(
            [CC, "-std=c11", "-Wall", "-Wextra", "-Werror",
             str(harness), "-o", str(cls.binary), "-lm"],
            capture_output=True, text=True,
        )
        if result.returncode != 0:
            raise AssertionError(f"harness did not compile:\n{result.stderr}")

    def test_extracted_helpers(self):
        result = subprocess.run([str(self.binary)], capture_output=True,
                                text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("harness ok", result.stdout)


@unittest.skipUnless(dbus_available(), "dbus-1 development files are required")
class CBinaryArgumentTests(unittest.TestCase):
    """The real binary's argument surface, built with -Werror."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="linkr-c-build-")
        cls.addClassCleanup(cls.tmp.cleanup)
        directory = Path(cls.tmp.name)
        binary = directory / "linkr_ble_terminal_c"
        flags = subprocess.run(["pkg-config", "--cflags", "dbus-1"],
                               capture_output=True, text=True, check=True).stdout
        libs = subprocess.run(["pkg-config", "--libs", "dbus-1"],
                              capture_output=True, text=True, check=True).stdout
        command = [CC, "-O2", "-Wall", "-Wextra", "-Werror", "-std=c11",
                   "-D_GNU_SOURCE", *shlex.split(flags),
                   str(SOURCE_PATH), "-o", str(binary),
                   *shlex.split(libs), "-lpthread"]
        result = subprocess.run(command, capture_output=True, text=True)
        if result.returncode != 0:
            raise AssertionError(f"the C client did not build:\n{result.stderr}")
        cls.binary = binary

    def run_cli(self, *arguments):
        return subprocess.run([str(self.binary), *arguments],
                              capture_output=True, text=True, timeout=30,
                              stdin=subprocess.DEVNULL)

    def test_help_goes_to_stdout_and_documents_the_new_flags(self):
        result = self.run_cli("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Usage:", result.stdout)
        for flag in ("--version", "--quiet", "--option=value"):
            self.assertIn(flag, result.stdout)
        self.assertEqual(result.stderr, "")

    def test_version_goes_to_stdout(self):
        result = self.run_cli("--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(c_version_string(), result.stdout)

    def test_unknown_option_exits_two_with_a_hint(self):
        result = self.run_cli("--nope")
        self.assertEqual(result.returncode, 2)
        self.assertIn("unknown option", result.stderr)
        self.assertIn("--help", result.stderr)

    def test_missing_value_is_named_instead_of_unknown_option(self):
        for flag in ("--name", "--address", "--timeout", "--escape",
                     "--log-file", "--ble-write-size"):
            with self.subTest(flag=flag):
                result = self.run_cli(flag)
                self.assertEqual(result.returncode, 2)
                self.assertIn(f"{flag} requires a value", result.stderr)
                self.assertNotIn("unknown option", result.stderr)

    def test_value_flags_reject_a_bad_value_with_exit_two(self):
        cases = [("--timeout", "0"), ("--timeout", "abc"),
                 ("--loopback-timeout", "-1"), ("--ble-write-size", "999"),
                 ("--ble-write-size", "x"), ("--enter", "bogus"),
                 ("--escape", "^"), ("--escape", "0x1ff")]
        for flag, value in cases:
            with self.subTest(flag=flag, value=value):
                result = self.run_cli(flag, value)
                self.assertEqual(result.returncode, 2, result.stderr)

    def test_boolean_flags_reject_an_inline_value(self):
        for argument in ("--scan=1", "--quiet=yes", "--no-terminal=true"):
            with self.subTest(argument=argument):
                result = self.run_cli(argument)
                self.assertEqual(result.returncode, 2)

    def test_inline_and_separate_values_are_equivalent(self):
        for arguments in (("--name=Phone", "--timeout=2.5", "--version"),
                          ("--name", "Phone", "--timeout", "2.5", "--version")):
            with self.subTest(arguments=arguments):
                result = self.run_cli(*arguments)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(c_version_string(), result.stdout)

    def test_quiet_only_silences_progress(self):
        # A bad argument is never progress chatter: it must still be reported.
        result = self.run_cli("--quiet", "--nope")
        self.assertEqual(result.returncode, 2)
        self.assertIn("unknown option", result.stderr)

    def test_unwritable_log_file_fails_before_any_bluetooth(self):
        result = self.run_cli("--log-file", "/nonexistent-dir/linkr.log",
                              "--no-terminal")
        self.assertEqual(result.returncode, 1)
        self.assertIn("cannot open log file", result.stderr)
        # It must fail on the log file, not after a failed D-Bus or scan.
        self.assertNotIn("D-Bus", result.stderr)
        self.assertNotIn("BlueZ", result.stderr)

    def completion(self, shell):
        result = self.run_cli("--print-completion", shell)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.strip(), "empty completion script")
        return result.stdout

    @staticmethod
    def bash_option_words(script):
        """The option list the generated bash function completes with."""
        lines = script.splitlines()
        index = next(i for i, line in enumerate(lines)
                     if line.strip().startswith('if [[ "$cur" == -* ]]'))
        return re.findall(r"-{1,2}[a-z][a-z-]*", lines[index + 1])

    @classmethod
    def parsed_flags(cls):
        """Every flag parse_args() matches on, read from the source."""
        return set(re.findall(r'strcmp\(flag, "(-{1,2}[a-z][a-z-]*)"\)',
                              SOURCE_PATH.read_text()))

    def test_completion_lists_exactly_the_options_the_parser_accepts(self):
        self.assertEqual(set(self.bash_option_words(self.completion("bash"))),
                         self.parsed_flags())

    def test_completion_scripts_parse_as_their_shell(self):
        for shell in ("bash", "fish", "zsh"):
            with self.subTest(shell=shell):
                if not shutil.which(shell):
                    self.skipTest(f"{shell} is not installed")
                script = self.completion(shell)
                with tempfile.TemporaryDirectory(prefix="linkr-c-completion-") as d:
                    path = Path(d) / f"completion.{shell}"
                    path.write_text(script)
                    # fish spells the syntax check differently.
                    command = (["fish", "--no-execute", str(path)]
                               if shell == "fish" else [shell, "-n", str(path)])
                    check = subprocess.run(command, capture_output=True, text=True)
                self.assertEqual(check.returncode, 0, check.stderr)

    def test_completion_fish_registers_every_option(self):
        lines = [line for line in self.completion("fish").splitlines()
                 if line.startswith("complete ")]
        flags = self.parsed_flags()
        self.assertEqual(len(lines), len(flags))
        for flag in sorted(flags):
            expected = f"-l {flag[2:]}" if flag.startswith("--") else f"-s {flag[1:]}"
            with self.subTest(flag=flag):
                self.assertTrue(any(expected in line for line in lines),
                                f"fish script does not register {flag}")

    def test_completion_fish_marks_choices_and_files(self):
        fish = self.completion("fish")
        self.assertIn("-l enter -x -a 'raw cr lf crlf'", fish)
        self.assertIn("-l print-completion -x -a 'bash fish zsh'", fish)
        self.assertIn("-l log-file -r -F", fish)
        self.assertIn("-l name -r -f", fish)

    def test_completion_marks_value_options(self):
        bash = self.completion("bash")
        self.assertIn('--enter) COMPREPLY=( $(compgen -W "raw cr lf crlf"', bash)
        self.assertIn("--log-file) COMPREPLY=( $(compgen -f", bash)
        zsh = self.completion("zsh")
        self.assertIn("--log-file=[append raw BLE RX bytes to file]:file:_files",
                      zsh)
        self.assertIn(":enter:(raw cr lf crlf)", zsh)

    def test_completion_registers_the_command_name(self):
        self.assertIn("complete -F _linkr_ble_terminal_c linkr_ble_terminal_c",
                      self.completion("bash"))

    @unittest.skipUnless(shutil.which("bash"), "bash is required")
    def test_bash_completion_answers_the_way_a_shell_needs(self):
        with tempfile.TemporaryDirectory(prefix="linkr-c-completion-") as d:
            path = Path(d) / "completion.bash"
            path.write_text(self.completion("bash"))
            probes = [("--e", ["--enter", "--escape"]),
                      ("--enter", "l", ["lf"]),
                      ("--enter=l", ["--enter=lf"]),
                      ("--print-completion", "z", ["zsh"]),
                      ("--print-completion=z", ["--print-completion=zsh"])]
            for probe in probes:
                with self.subTest(probe=probe[:-1]):
                    words = " ".join(shlex.quote(word) for word in probe[:-1])
                    program = "\n".join([
                        f"source {shlex.quote(str(path))}",
                        f"COMP_WORDS=(linkr_ble_terminal_c {words})",
                        "COMP_CWORD=$(( ${#COMP_WORDS[@]} - 1 ))",
                        "COMPREPLY=()",
                        "_linkr_ble_terminal_c",
                        'printf "%s\\n" "${COMPREPLY[@]}"',
                    ])
                    result = subprocess.run(["bash", "-c", program],
                                            capture_output=True, text=True,
                                            timeout=30)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout.split(), probe[-1])

    def test_unsupported_shell_is_a_usage_error(self):
        result = self.run_cli("--print-completion", "ksh")
        self.assertEqual(result.returncode, 2)
        self.assertIn("must be bash, fish or zsh", result.stderr)


class CParityTests(unittest.TestCase):
    """Cheap guards that the two host clients keep the same contract."""

    @classmethod
    def setUpClass(cls):
        cls.c_source = SOURCE_PATH.read_text()
        cls.py_source = PYTHON_PATH.read_text()

    def test_version_strings_match(self):
        c_version = re.search(r'#define CLI_VERSION\s+"([^"]+)"', self.c_source)
        py_version = re.search(r'^__version__ = "([^"]+)"', self.py_source,
                               re.MULTILINE)
        self.assertIsNotNone(c_version, "CLI_VERSION missing from the C client")
        self.assertIsNotNone(py_version, "__version__ missing from the CLI")
        self.assertEqual(c_version.group(1), py_version.group(1))

    def test_exit_codes_match(self):
        for name in ("EXIT_OK", "EXIT_ERROR", "EXIT_USAGE",
                     "EXIT_DEVICE_DISCONNECTED"):
            with self.subTest(name=name):
                c_value = re.search(rf"{name}\s*=\s*(\d+)", self.c_source)
                py_value = re.search(rf"^{name}\s*=\s*(\d+)", self.py_source,
                                     re.MULTILINE)
                self.assertIsNotNone(c_value, f"{name} missing from the C client")
                self.assertIsNotNone(py_value, f"{name} missing from the CLI")
                self.assertEqual(c_value.group(1), py_value.group(1))

    def test_escape_error_message_matches(self):
        message = "escape must be one byte, like ^] or 0x1d"
        self.assertIn(message, self.c_source)
        self.assertIn(message, self.py_source)

    def test_both_clients_list_unnamed_devices(self):
        # Dropping devices without a Name property makes an empty scan look like
        # "nothing is nearby"; both clients must label them instead.
        self.assertIn("(unknown)", self.c_source)
        self.assertIn("(unknown)", self.py_source)

    def test_both_clients_report_the_same_disconnect_message(self):
        message = "BLE disconnected during the terminal session"
        self.assertIn(message, self.c_source)
        self.assertIn(message, self.py_source)

    def test_both_clients_keep_the_scan_and_exit_rule(self):
        # C: main's early return; Python: has_control_action() plus --address.
        self.assertIn(
            "Listing is the whole job unless another action was requested",
            self.c_source)
        self.assertIn("def has_control_action(", self.py_source)

    def test_completion_table_covers_every_flag_parse_args_matches(self):
        """The C completions come from a hand-kept table; this ties it to the
        parser so a new option cannot be added to one and not the other."""
        table = set(re.findall(r'\{\s*"(-{1,2}[a-z][a-z-]*)",\s*(?:true|false)',
                               self.c_source))
        parsed = set(re.findall(r'strcmp\(flag, "(-{1,2}[a-z][a-z-]*)"\)',
                                self.c_source))
        self.assertTrue(parsed)
        self.assertEqual(table, parsed)

    def test_completion_shells_match(self):
        py_shells = set(re.findall(r'"([a-z]+)"', re.search(
            r"COMPLETION_SHELLS = \(([^)]*)\)", self.py_source).group(1)))
        c_choices = re.search(
            r'"--print-completion",\s*true,\s*false,\s*"([^"]*)"',
            self.c_source).group(1)
        self.assertEqual(py_shells, set(c_choices.split()))

    def test_both_clients_expose_print_completion(self):
        for source in (self.c_source, self.py_source):
            self.assertIn("--print-completion", source)


if __name__ == "__main__":
    unittest.main()
