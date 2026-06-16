/*
 * ztest_sections.ld — Linker script fragment for Zephyr ztest section markers.
 *
 * The Zephyr ztest framework uses ELF section iterables (STRUCT_SECTION_ITERABLE)
 * to register test suites, test cases, expected results, and test rules.  These
 * are placed in subsections named ._ztest_<type>.static.<varname>_ and the
 * framework expects the linker to provide _ztest_<type>_list_start / _list_end
 * symbols that bracket all entries of each type.
 *
 * In a normal Zephyr build, the custom linker script defines these output sections
 * and their markers.  In our cc-crate build (cross-platform, no linker script),
 * the markers were previously stubbed to the same zero address, making all
 * iteration loops see zero entries.
 *
 * This fragment uses GNU ld's INSERT AFTER directive to define the output
 * sections without replacing the default linker script.  Each section groups
 * the relevant input sections with SORT_BY_NAME (matching Zephyr's convention)
 * and provides the start/end symbols the framework expects.
 *
 * This works on Linux (GNU ld / lld).  macOS Mach-O and Windows MSVC linkers
 * do not support INSERT; on those platforms the cc-crate build does not
 * enable ztest mode (ZEPHYR_APP=ztest is Linux-only for now).
 */
SECTIONS {
  _ztest_unit_test_list_area : {
    PROVIDE(_ztest_unit_test_list_start = .);
    KEEP(*(SORT_BY_NAME(._ztest_unit_test.static.*)))
    PROVIDE(_ztest_unit_test_list_end = .);
  }

  _ztest_suite_node_list_area : {
    PROVIDE(_ztest_suite_node_list_start = .);
    KEEP(*(SORT_BY_NAME(._ztest_suite_node.static.*)))
    PROVIDE(_ztest_suite_node_list_end = .);
  }

  _ztest_expected_result_entry_list_area : {
    PROVIDE(_ztest_expected_result_entry_list_start = .);
    KEEP(*(SORT_BY_NAME(._ztest_expected_result_entry.static.*)))
    PROVIDE(_ztest_expected_result_entry_list_end = .);
  }

  _ztest_test_rule_list_area : {
    PROVIDE(_ztest_test_rule_list_start = .);
    KEEP(*(SORT_BY_NAME(._ztest_test_rule.static.*)))
    PROVIDE(_ztest_test_rule_list_end = .);
  }
}
INSERT AFTER .data;
