# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

function Get-BackendEvidencePolicy([bool]$IsScreening) {
    return [pscustomobject][ordered]@{
        evidence_grade = if ($IsScreening) { "screening" } else { "final" }
        measured_pairs = if ($IsScreening) { 3 } else { 6 }
        final_eligible = -not $IsScreening
    }
}

function Get-FailedBackendSurvivalComponents([Collections.IDictionary]$AggregateVerdicts) {
    return @(
        @("equal_work", "calibration", "backend_health", "compatibility") |
            Where-Object { $AggregateVerdicts[$_] -ne "pass" }
    )
}

function Get-BackendQuakeFinalTerminationReasons([string]$Role, [object[]]$Samples) {
    $reasons = @()
    foreach ($sample in $Samples) {
        $stopIdentity = Get-StopIdentityKey $sample
        if ($stopIdentity -cne "test_exit|code=0|requested=|message=") {
            $reasons += "$Role $($sample.gate_observation) did not reach Lotura TestExit code 0 after the Quake timedemo (observed $stopIdentity)"
        }
    }
    return @($reasons)
}

function Get-BackendCompatibilityReasons($Policy, [string]$Role, [object[]]$Samples) {
    $reasons = @()
    if ($Policy.name -eq "quake-586") {
        $reasons += @(Get-BackendQuakeFinalTerminationReasons $Role $Samples)
    }
    foreach ($sample in $Samples) {
        $label = "$Role $($sample.gate_observation)"
        if ($sample.gate_process_exit_code -ne 0) {
            $reasons += "$label host exit code is $($sample.gate_process_exit_code)"
        }
        if ($sample.gate_artifacts.result_block_status -ne "valid" -or
            $sample.gate_artifacts.result_block_count -ne 1 -or
            [string]$sample.gate_artifacts.result_block_sha256 -notmatch '^[0-9a-f]{64}$') {
            $reasons += "$label did not produce exactly one hashable semantic result block"
        }
        if ($Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
            if ((Get-StopIdentityKey $sample) -cne "test_exit|code=0|requested=|message=") {
                $reasons += "$label did not reach Lotura TestExit code 0"
            }
            if ($null -eq $sample.timedemo -or $sample.timedemo.gametics -ne 2134 -or
                $sample.timedemo.realtics -le 0) {
                $reasons += "$label did not report the 2134-gametic Doom demo"
            }
        } else {
            $completion = if ($null -ne $sample.PSObject.Properties["gate_quake_completion"]) {
                $sample.gate_quake_completion
            } else {
                $null
            }
            $fixture = if ($null -ne $sample.PSObject.Properties["gate_fixture"]) {
                $sample.gate_fixture
            } else {
                $null
            }
            $reasons += @(Get-BackendQuakeCompletionReasons $completion $label)
            $reasons += @(Get-BackendQuakeFixtureReasons $fixture $label)
        }
    }
    return @($reasons)
}

function Get-BackendTerminationProjection($Policy, [object[]]$Automatic, [object[]]$Interpreter) {
    $compatibilityReasons = @(
        @(Get-BackendCompatibilityReasons $Policy "automatic" $Automatic) +
        @(Get-BackendCompatibilityReasons $Policy "interpreter" $Interpreter)
    )
    $finalTerminationReasons = @()
    if ($Policy.name -eq "quake-586") {
        $finalTerminationReasons = @(
            @(Get-BackendQuakeFinalTerminationReasons "automatic" $Automatic) +
            @(Get-BackendQuakeFinalTerminationReasons "interpreter" $Interpreter)
        )
    }
    return [pscustomobject][ordered]@{
        compatibility_verdict = if ($compatibilityReasons.Count -eq 0) { "pass" } else { "fail" }
        compatibility_reasons = [object[]]$compatibilityReasons
        final_termination_reasons = [object[]]$finalTerminationReasons
    }
}

function Get-BackendFinalTerminationReasonsFromWorkloads([object[]]$Workloads) {
    $reasons = @()
    foreach ($workload in $Workloads) {
        foreach ($reason in @($workload.checks.final_termination.failure_reasons)) {
            if (-not [string]::IsNullOrWhiteSpace([string]$reason)) {
                $reasons += [string]$reason
            }
        }
    }
    return @($reasons)
}

function Get-BackendFinalClassification(
    [bool]$IsScreening,
    [bool]$BaseFinalEligible,
    [int]$FinalTerminationFailureCount,
    [bool]$SurvivalPassed
) {
    $finalEligible = $BaseFinalEligible -and $FinalTerminationFailureCount -eq 0
    $trackASurvival = if ($IsScreening) {
        "not_evaluated"
    } elseif (-not $finalEligible) {
        "ineligible"
    } elseif ($SurvivalPassed) {
        "pass"
    } else {
        "fail"
    }
    $verdict = if ($IsScreening) {
        "screening"
    } elseif ($trackASurvival -eq "pass") {
        "survived"
    } elseif ($trackASurvival -eq "ineligible") {
        "ineligible"
    } else {
        "failed"
    }
    return [pscustomobject][ordered]@{
        final_eligible = $finalEligible
        track_a_survival = $trackASurvival
        verdict = $verdict
    }
}

function Get-RoleSummary(
    [string]$Name,
    [string]$Mode,
    [object[]]$Samples,
    [bool]$EnforceDeterminism = $true
) {
    if ($EnforceDeterminism) {
        Assert-RoleDeterminism $Name $Samples
    }
    return [ordered]@{
        name = $Name
        mode = $Mode
        runs = $Samples
        median = [ordered]@{
            wall_seconds = Get-Median ([double[]]$Samples.wall_seconds)
            guest_seconds = Get-Median ([double[]]$Samples.guest_seconds)
            real_time_factor = Get-Median ([double[]]$Samples.real_time_factor)
            instructions_per_host_second = Get-Median ([double[]]$Samples.instructions_per_host_second)
            direct_native_coverage = Get-Median ([double[]]$Samples.direct_native_coverage)
            direct_slow_exits_per_100_instructions = Get-Median ([double[]]$Samples.direct_slow_exits_per_100_instructions)
        }
    }
}

function Get-PairedWorkloadSummary($Policy, [object[]]$Candidate, [object[]]$Baseline) {
    if ($Candidate.Count -ne $Baseline.Count) {
        throw "$($Policy.name) has incomplete pairs."
    }
    $ipsRatios = @()
    $rtfRatios = @()
    $pairs = for ($index = 0; $index -lt $Candidate.Count; $index++) {
        $ipsRatio = $Candidate[$index].instructions_per_host_second / $Baseline[$index].instructions_per_host_second
        $rtfRatio = $Candidate[$index].real_time_factor / $Baseline[$index].real_time_factor
        $ipsRatios += $ipsRatio
        $rtfRatios += $rtfRatio
        [ordered]@{
            pair = $index + 1
            ips_ratio = $ipsRatio
            real_time_factor_ratio = $rtfRatio
        }
    }
    $candidateChecks = Get-CandidateSampleChecks $Policy $Candidate
    return [ordered]@{
        name = $Policy.name
        mode = $Policy.mode
        minimum_real_time_factor = $Policy.minimum_real_time_factor
        candidate = Get-RoleSummary $Policy.name $Policy.mode $Candidate
        baseline = Get-RoleSummary $Policy.name $Policy.mode $Baseline
        pairs = $pairs
        paired_metrics = [ordered]@{
            instructions_per_host_second = Get-PairedMetric ([double[]]$ipsRatios)
            real_time_factor = Get-PairedMetric ([double[]]$rtfRatios)
        }
        candidate_sample_checks = $candidateChecks
        candidate_floor_passes = $candidateChecks.real_time_floor_passes
    }
}

function Get-BackendCalibrationReasons($Policy, [string]$Role, [object[]]$Samples) {
    $reasons = @()
    foreach ($sample in $Samples) {
        $label = "$Role $($sample.gate_observation)"
        if ($Policy.name.StartsWith("doom-", [StringComparison]::Ordinal)) {
            if ($null -eq $sample.timedemo -or
                $sample.timedemo.realtics -lt $Policy.minimum_realtics -or
                $sample.timedemo.realtics -gt $Policy.maximum_realtics) {
                $reasons += "$label is outside the Doom realtics calibration band"
            }
        } elseif ($null -eq $sample.quake_timedemo -or
            $sample.quake_timedemo.fps -lt 41.0 -or
            $sample.quake_timedemo.fps -gt 44.0) {
            $reasons += "$label is outside the Quake 41-44 fps calibration band"
        }
    }
    return @($reasons)
}

function Get-BackendSelectionReasons(
    [object[]]$Automatic,
    [object[]]$Interpreter
) {
    $reasons = @()
    foreach ($sample in $Automatic) {
        if ($sample.perf.jit_direct_entries -le 0 -or $sample.perf.jit_direct_insns -le 0) {
            $reasons += "automatic $($sample.gate_observation) did not execute the direct backend"
        }
    }
    $zeroFields = @(
        "jit_region_entries", "jit_region_insns", "jit_native_insns",
        "jit_direct_entries", "jit_direct_insns"
    )
    foreach ($sample in $Interpreter) {
        foreach ($field in $zeroFields) {
            if ($sample.perf.$field -ne 0) {
                $reasons += "interpreter $($sample.gate_observation) reported nonzero $field"
            }
        }
    }
    return @($reasons)
}

function Get-BackendWorkloadSummary(
    $Policy,
    [object[]]$Automatic,
    [object[]]$Interpreter,
    [bool]$IsScreening
) {
    if ($Automatic.Count -ne $Interpreter.Count) {
        throw "$($Policy.name) has incomplete backend pairs."
    }
    $ipsRatios = @()
    $rtfRatios = @()
    $equalWorkFailures = @()
    $pairs = for ($index = 0; $index -lt $Automatic.Count; $index++) {
        $ipsRatio = $Automatic[$index].instructions_per_host_second /
            $Interpreter[$index].instructions_per_host_second
        $rtfRatio = $Automatic[$index].real_time_factor /
            $Interpreter[$index].real_time_factor
        $ipsRatios += $ipsRatio
        $rtfRatios += $rtfRatio
        $automaticWork = Get-EqualWorkRecord $Policy.name $Automatic[$index]
        $interpreterWork = Get-EqualWorkRecord $Policy.name $Interpreter[$index]
        $equalWork = Compare-EqualWorkRecords $automaticWork $interpreterWork
        if ($Automatic[$index].gate_artifacts.result_block_status -ne "valid" -or
            $Interpreter[$index].gate_artifacts.result_block_status -ne "valid") {
            $equalWork = [pscustomobject][ordered]@{
                matches = $false
                mismatched_fields = @(
                    @($equalWork.mismatched_fields) + "result_block_identity" |
                        Sort-Object -Unique
                )
            }
            $equalWorkFailures += "pair $($index + 1): semantic result block is invalid"
        }
        if (-not $equalWork.matches) {
            $equalWorkFailures += "pair $($index + 1): $($equalWork.mismatched_fields -join ', ')"
        }
        [ordered]@{
            pair = $index + 1
            automatic_observation = $Automatic[$index].gate_observation
            interpreter_observation = $Interpreter[$index].gate_observation
            ips_ratio = $ipsRatio
            real_time_factor_ratio = $rtfRatio
            equal_work = $equalWork
        }
    }

    $automaticDeterminism = Get-RoleExactDeterminism $Policy.name $Automatic
    $interpreterDeterminism = Get-RoleExactDeterminism $Policy.name $Interpreter
    if (-not $automaticDeterminism.deterministic) {
        $equalWorkFailures += "automatic role: $($automaticDeterminism.mismatched_fields -join ', ')"
    }
    if (-not $interpreterDeterminism.deterministic) {
        $equalWorkFailures += "interpreter role: $($interpreterDeterminism.mismatched_fields -join ', ')"
    }

    $terminationProjection = Get-BackendTerminationProjection $Policy $Automatic $Interpreter
    $compatibilityReasons = @($terminationProjection.compatibility_reasons)
    $finalTerminationReasons = @($terminationProjection.final_termination_reasons)
    $calibrationReasons = @(
        @(Get-BackendCalibrationReasons $Policy "automatic" $Automatic) +
        @(Get-BackendCalibrationReasons $Policy "interpreter" $Interpreter)
    )
    $backendReasons = @(Get-BackendSelectionReasons $Automatic $Interpreter)
    $ipsMetric = Get-BackendPairedMetric ([double[]]$ipsRatios)
    $rtfMetric = Get-BackendPairedMetric ([double[]]$rtfRatios)
    $survivalReasons = @()
    if ($ipsMetric.verdict -ne "pass") {
        $survivalReasons += "IPS survival threshold failed"
    }
    if ($rtfMetric.verdict -ne "pass") {
        $survivalReasons += "real-time-factor survival threshold failed"
    }

    $requiredFloorPasses = if ($IsScreening) { 2 } else { $minimumFloorPasses }
    $productFloorPasses = @($Automatic | Where-Object {
        $_.real_time_factor -ge $Policy.minimum_real_time_factor
    }).Count
    $productReasons = @()
    if ($productFloorPasses -lt $requiredFloorPasses) {
        $productReasons += "$productFloorPasses of $($Automatic.Count) automatic samples meet the product floor"
    }

    return [ordered]@{
        name = $Policy.name
        mode = $Policy.mode
        configuration = [ordered]@{
            cpu_mode = $Policy.mode
            memory_mib = 24
            video = "vega"
            cycle_budget = $Policy.cycle_budget
        }
        termination_policy = if ($Policy.name -eq "quake-586") {
            "one_969_frame_timedemo_then_post_demo_wait_then_lotura_test_exit_code_0"
        } else {
            "lotura_test_exit_after_timedemo"
        }
        final_termination_policy = if ($Policy.name -eq "quake-586") {
            "one_969_frame_timedemo_then_lotura_test_exit_code_0"
        } else {
            "lotura_test_exit_code_0"
        }
        minimum_real_time_factor = $Policy.minimum_real_time_factor
        automatic = Get-RoleSummary "automatic" $Policy.mode $Automatic $false
        interpreter = Get-RoleSummary "interpreter" $Policy.mode $Interpreter $false
        pairs = $pairs
        paired_metrics = [ordered]@{
            instructions_per_host_second = $ipsMetric
            real_time_factor = $rtfMetric
        }
        survival = [ordered]@{
            verdict = if ($survivalReasons.Count -eq 0) { "pass" } else { "fail" }
            failure_reasons = $survivalReasons
        }
        equal_work = [ordered]@{
            verdict = if ($equalWorkFailures.Count -eq 0) { "pass" } else { "fail" }
            automatic_role_determinism = $automaticDeterminism
            interpreter_role_determinism = $interpreterDeterminism
            failure_reasons = $equalWorkFailures
        }
        verdicts = [ordered]@{
            product = if ($productReasons.Count -eq 0) { "pass" } else { "fail" }
            equal_work = if ($equalWorkFailures.Count -eq 0) { "pass" } else { "fail" }
            calibration = if ($calibrationReasons.Count -eq 0) { "pass" } else { "fail" }
            backend_health = if ($backendReasons.Count -eq 0) { "pass" } else { "fail" }
            compatibility = $terminationProjection.compatibility_verdict
        }
        checks = [ordered]@{
            product = [ordered]@{
                required_floor_passes = $requiredFloorPasses
                actual_floor_passes = $productFloorPasses
                failure_reasons = $productReasons
            }
            calibration = [ordered]@{ failure_reasons = $calibrationReasons }
            backend_health = [ordered]@{ failure_reasons = $backendReasons }
            compatibility = [ordered]@{ failure_reasons = $compatibilityReasons }
            final_termination = [ordered]@{ failure_reasons = $finalTerminationReasons }
        }
    }
}
