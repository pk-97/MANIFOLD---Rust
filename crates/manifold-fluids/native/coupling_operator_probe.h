#pragma once

// Small independent dense oracle for the sparse rigid-pressure extension.
// Called under the bridge's existing native guard, only by the CPU test fixture.
void run_coupling_operator_probe();
