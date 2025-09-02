/*
 * Copyright The Rusted Firmware-A Contributors.
 * Copyright (c) 2013-2019, ARM Limited and Contributors. All rights reserved.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */

.global	plat_panic_handler
.global	plat_handle_el3_ea
.globl	plat_my_core_pos

	/* -----------------------------------------------------
	 * Endless loop by default.
	 * -----------------------------------------------------
	 */
func plat_panic_handler
	wfi
	b	plat_panic_handler
endfunc plat_panic_handler

	/* -----------------------------------------------------
	 * Platform handler for EL3 External Abort.
	 * -----------------------------------------------------
	 */
func plat_handle_el3_ea
	b	report_unhandled_exception
endfunc plat_handle_el3_ea

	/* -----------------------------------------------------
	 *  unsigned int plat_my_core_pos(void)
	 *  This function uses the plat_calc_core_pos()
	 *  definition to get the index of the calling CPU.
	 *  clobbers: x0-x5
	 * -----------------------------------------------------
	 */
func plat_my_core_pos
	mrs	x0, mpidr_el1
	b	{plat_calc_core_pos}
endfunc plat_my_core_pos
