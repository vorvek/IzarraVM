# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

function Assert-TrackMComparisonMode(
    [bool]$IsBackendBakeoff,
    [bool]$IsReportOnly,
    [bool]$HasExplicitBaseline,
    [bool]$HasExplicitJit,
    [bool]$HasExplicitExecutable,
    [bool]$SkipRequested,
    [string]$RequestedExecutionRole,
    [bool]$IsScreening,
    [bool]$HasExplicitRuns,
    [int]$RunCount,
    [string]$WorkloadSelection,
    [int]$RequestedProcessorIndex,
    [string]$LockPath
) {
    if ($IsBackendBakeoff -or $IsReportOnly) {
        throw "Track M comparison cannot be combined with BackendBakeoff or ReportOnly."
    }
    if ($HasExplicitBaseline -or $HasExplicitJit -or $HasExplicitExecutable -or $SkipRequested) {
        throw "Track M comparison derives its parent, builds both revisions, and forces its execution policy."
    }
    $normalizedRole = $RequestedExecutionRole.Trim().ToLowerInvariant()
    if ($normalizedRole -notin @("automatic", "interpreter")) {
        throw "Track M comparison requires ExecutionRole automatic or interpreter."
    }
    if ($WorkloadSelection -cne "Both") {
        throw "Track M comparison requires all three workloads."
    }
    if ($RequestedProcessorIndex -ne 8) {
        throw "Track M comparison requires ProcessorIndex 8."
    }
    if ([string]::IsNullOrWhiteSpace($LockPath) -or
        -not [IO.Path]::IsPathFullyQualified($LockPath)) {
        throw "Track M comparison requires an absolute MeasurementLockPath."
    }
    if ($IsScreening) {
        if ($HasExplicitRuns -and $RunCount -ne 3) {
            throw "Track M screening requires exactly three measured pairs."
        }
    } elseif ($RunCount -ne 6) {
        throw "Track M confirmation requires exactly six measured pairs."
    }
}

function Get-TrackMExecutionPolicy([string]$RequestedExecutionRole) {
    switch ($RequestedExecutionRole.Trim().ToLowerInvariant()) {
        "automatic" {
            return [pscustomobject][ordered]@{
                name = "automatic"
                cli = "default automatic backend"
                jit = "1"
            }
        }
        "interpreter" {
            return [pscustomobject][ordered]@{
                name = "interpreter"
                cli = "--interpreter"
                jit = "0"
            }
        }
        default { throw "Unknown Track M execution role '$RequestedExecutionRole'." }
    }
}

function Get-TrackMParentFromRevisionLine([string]$Line, [string]$CandidateCommit) {
    $tokens = @($Line.Trim() -split '\s+' | Where-Object { $_ -ne "" })
    if ($tokens.Count -eq 0 -or $tokens[0] -notmatch '^[0-9a-fA-F]{40,64}$' -or
        $tokens[0] -cne $CandidateCommit) {
        throw "Track M could not verify the candidate revision while deriving its parent."
    }
    if ($tokens.Count -eq 1) {
        throw "Track M cannot compare a root commit with an immediate parent."
    }
    if ($tokens.Count -ne 2) {
        throw "Track M requires a candidate commit with exactly one immediate parent."
    }
    if ($tokens[1] -notmatch '^[0-9a-fA-F]{40,64}$') {
        throw "Track M received an invalid immediate parent revision."
    }
    return $tokens[1].ToLowerInvariant()
}

function Get-TrackMImmediateParent([string]$RepositoryRoot, [string]$CandidateCommit) {
    $revisionLine = @(& git -C $RepositoryRoot rev-list --parents -n 1 $CandidateCommit 2>$null)
    if ($LASTEXITCODE -ne 0 -or $revisionLine.Count -ne 1) {
        throw "Track M could not read the candidate commit's immediate parent."
    }
    return Get-TrackMParentFromRevisionLine $revisionLine[0] $CandidateCommit
}

function Get-TrackMPairedMetric([double[]]$Ratios) {
    $metric = Get-PairedMetric $Ratios
    $verdict = Get-TrackMPairedMetricVerdict $metric.median_ratio $metric.lower_95_ratio
    return [pscustomobject][ordered]@{
        median_ratio = $metric.median_ratio
        lower_95_ratio = $metric.lower_95_ratio
        lower_bound_confidence = $metric.lower_bound_confidence
        required_median_ratio = 0.99
        required_lower_95_ratio = 0.97
        verdict = $verdict
    }
}

function Get-TrackMPairedMetricVerdict([double]$Median, [double]$Lower95) {
    if ([double]::IsNaN($Median) -or [double]::IsInfinity($Median) -or
        [double]::IsNaN($Lower95) -or [double]::IsInfinity($Lower95) -or
        $Median -le 0 -or $Lower95 -le 0) {
        throw "Track M paired metric verdict inputs must be finite and positive."
    }
    if ($Median -lt 0.99) {
        return "regression"
    }
    if ($Lower95 -lt 0.97) {
        return "inconclusive"
    }
    return "pass"
}

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

function Get-TrackMSampleProvenanceReasons(
    $Policy,
    [string]$RevisionRole,
    [string]$ExpectedObservation,
    $Sample,
    $ExecutionPolicy
) {
    $label = "$RevisionRole $ExpectedObservation"
    $reasons = @()
    if ($null -eq $Sample) {
        return @("$label sample is missing")
    }
    foreach ($property in @(
        "gate_role", "gate_observation", "gate_processor_index",
        "gate_processor_affinity_mask", "gate_processor_affinity_verified",
        "gate_execution_role", "gate_execution_cli", "gate_execution_jit",
        "gate_measurement_fixture_sha256", "gate_termination_policy", "gate_artifacts"
    )) {
        if ($null -eq $Sample.PSObject.Properties[$property]) {
            $reasons += "$label is missing $property"
        }
    }
    if ($reasons.Count -ne 0) {
        return @($reasons)
    }
    if ($Sample.gate_role -cne $RevisionRole -or
        $Sample.gate_observation -cne $ExpectedObservation) {
        $reasons += "$label has the wrong revision role or observation identity"
    }
    $expectedMask = Format-AffinityMask ([int64]1 -shl 8)
    if ($Sample.gate_processor_index -ne 8 -or
        $Sample.gate_processor_affinity_mask -cne $expectedMask -or
        -not $Sample.gate_processor_affinity_verified) {
        $reasons += "$label is missing verified processor 8 affinity"
    }
    if ($Sample.gate_execution_role -cne $ExecutionPolicy.name -or
        $Sample.gate_execution_cli -cne $ExecutionPolicy.cli -or
        [string]$Sample.gate_execution_jit -cne $ExecutionPolicy.jit) {
        $reasons += "$label did not use the forced $($ExecutionPolicy.name) execution policy"
    }
    if ([string]$Sample.gate_measurement_fixture_sha256 -notmatch '^[0-9a-f]{64}$') {
        $reasons += "$label is missing its frozen measurement fixture hash"
    }
    if ($Sample.gate_termination_policy -cne "lotura_test_exit") {
        $reasons += "$label has the wrong termination policy"
    }
    $artifacts = $Sample.gate_artifacts
    foreach ($property in @(
        "profile_json_file", "profile_json_sha256", "stdout_file", "stdout_sha256",
        "stderr_file", "stderr_sha256", "qconsole_file", "qconsole_sha256",
        "result_block_status", "result_block_count", "result_block_sha256",
        "result_block_normalized_bytes"
    )) {
        if ($null -eq $artifacts.PSObject.Properties[$property]) {
            $reasons += "$label artifact evidence is missing $property"
        }
    }
    if ($reasons.Count -ne 0) {
        return @($reasons)
    }
    foreach ($name in @("profile_json", "stdout", "stderr")) {
        $fileProperty = "${name}_file"
        $hashProperty = "${name}_sha256"
        $fileName = [string]$artifacts.$fileProperty
        if ([string]::IsNullOrWhiteSpace($fileName) -or
            [IO.Path]::GetFileName($fileName) -cne $fileName -or
            [string]$artifacts.$hashProperty -notmatch '^[0-9a-f]{64}$') {
            $reasons += "$label has invalid $name artifact evidence"
        }
    }
    if ($Policy.name -ceq "quake-586") {
        $qconsoleName = [string]$artifacts.qconsole_file
        if ([string]::IsNullOrWhiteSpace($qconsoleName) -or
            [IO.Path]::GetFileName($qconsoleName) -cne $qconsoleName -or
            [string]$artifacts.qconsole_sha256 -notmatch '^[0-9a-f]{64}$') {
            $reasons += "$label is missing its hashed QCONSOLE artifact"
        }
        if ($null -eq $Sample.gate_fixture -or
            $Sample.gate_measurement_fixture_sha256 -cne
                $Sample.gate_fixture.prelaunch_overridden_tree_sha256) {
            $reasons += "$label measurement fixture hash does not match its Quake fixture evidence"
        }
    } elseif ($null -ne $artifacts.qconsole_file -or $null -ne $artifacts.qconsole_sha256) {
        $reasons += "$label must keep explicit null QCONSOLE fields"
    }
    if ($artifacts.result_block_status -cne "valid" -or
        $artifacts.result_block_count -ne 1 -or
        [string]$artifacts.result_block_sha256 -notmatch '^[0-9a-f]{64}$' -or
        $artifacts.result_block_normalized_bytes -le 0) {
        $reasons += "$label is missing one complete hashed semantic result block"
    }
    if ($null -eq $Sample.PSObject.Properties["perf"] -or $null -eq $Sample.perf) {
        $reasons += "$label is missing performance counters"
        return @($reasons)
    }
    foreach ($field in @(
        "jit_region_entries", "jit_region_insns", "jit_native_insns",
        "jit_direct_entries", "jit_direct_insns", "jit_direct_side_exits"
    )) {
        if ($null -eq $Sample.perf.PSObject.Properties[$field]) {
            $reasons += "$label performance counters are missing $field"
        }
    }
    if ($reasons.Count -ne 0) {
        return @($reasons)
    }
    if ($ExecutionPolicy.name -ceq "automatic") {
        if ($Sample.perf.jit_direct_entries -le 0 -or $Sample.perf.jit_direct_insns -le 0) {
            $reasons += "$label did not execute the automatic direct backend"
        }
    } else {
        foreach ($property in $Sample.perf.PSObject.Properties) {
            if ($property.Name.StartsWith("jit_", [StringComparison]::Ordinal) -and
                $property.Value -ne 0) {
                $reasons += "$label interpreter evidence reported nonzero $($property.Name)"
            }
        }
    }
    return @($reasons)
}

function Get-TrackMWorkloadSummary(
    $Policy,
    [object[]]$Candidate,
    [object[]]$Parent,
    $WarmupBucket,
    $ExecutionPolicy
) {
    if ($Candidate.Count -ne $Parent.Count -or $Candidate.Count -notin @(3, 6)) {
        throw "$($Policy.name) requires three or six complete Track M pairs."
    }
    $candidateWarmups = @($WarmupBucket.candidate)
    $parentWarmups = @($WarmupBucket.parent)
    if ($candidateWarmups.Count -ne 1 -or $parentWarmups.Count -ne 1) {
        throw "$($Policy.name) requires one discarded warmup per revision role."
    }

    $candidateAll = @($candidateWarmups + $Candidate)
    $parentAll = @($parentWarmups + $Parent)
    $semanticReasons = @(
        @(Get-BackendCompatibilityReasons $Policy "candidate" $candidateAll) +
        @(Get-BackendCompatibilityReasons $Policy "parent" $parentAll)
    )
    $provenanceReasons = @()
    $provenanceReasons += @(Get-TrackMSampleProvenanceReasons `
        $Policy "candidate" "warmup" $candidateWarmups[0] $ExecutionPolicy)
    $provenanceReasons += @(Get-TrackMSampleProvenanceReasons `
        $Policy "parent" "warmup" $parentWarmups[0] $ExecutionPolicy)
    for ($index = 0; $index -lt $Candidate.Count; $index++) {
        $observation = "pair$($index + 1)"
        $provenanceReasons += @(Get-TrackMSampleProvenanceReasons `
            $Policy "candidate" $observation $Candidate[$index] $ExecutionPolicy)
        $provenanceReasons += @(Get-TrackMSampleProvenanceReasons `
            $Policy "parent" $observation $Parent[$index] $ExecutionPolicy)
    }

    $ipsRatios = @()
    $rtfRatios = @()
    $exactWorkReasons = @()
    $pairs = for ($index = 0; $index -lt $Candidate.Count; $index++) {
        $ipsRatio = $Candidate[$index].instructions_per_host_second /
            $Parent[$index].instructions_per_host_second
        $rtfRatio = $Candidate[$index].real_time_factor /
            $Parent[$index].real_time_factor
        $ipsRatios += $ipsRatio
        $rtfRatios += $rtfRatio
        $comparison = Compare-EqualWorkRecords `
            (Get-EqualWorkRecord $Policy.name $Candidate[$index]) `
            (Get-EqualWorkRecord $Policy.name $Parent[$index])
        if (-not $comparison.matches) {
            $exactWorkReasons += "pair $($index + 1): $($comparison.mismatched_fields -join ', ')"
        }
        [ordered]@{
            pair = $index + 1
            candidate_observation = $Candidate[$index].gate_observation
            parent_observation = $Parent[$index].gate_observation
            ips_ratio = $ipsRatio
            real_time_factor_ratio = $rtfRatio
            equal_work = $comparison
        }
    }
    $warmupComparison = Compare-EqualWorkRecords `
        (Get-EqualWorkRecord $Policy.name $candidateWarmups[0]) `
        (Get-EqualWorkRecord $Policy.name $parentWarmups[0])
    if (-not $warmupComparison.matches) {
        $exactWorkReasons += "warmup: $($warmupComparison.mismatched_fields -join ', ')"
    }
    $candidateDeterminism = Get-RoleExactDeterminism $Policy.name $candidateAll
    $parentDeterminism = Get-RoleExactDeterminism $Policy.name $parentAll
    if (-not $candidateDeterminism.deterministic) {
        $exactWorkReasons += "candidate role: $($candidateDeterminism.mismatched_fields -join ', ')"
    }
    if (-not $parentDeterminism.deterministic) {
        $exactWorkReasons += "parent role: $($parentDeterminism.mismatched_fields -join ', ')"
    }

    $ipsMetric = Get-TrackMPairedMetric ([double[]]$ipsRatios)
    $rtfMetric = Get-TrackMPairedMetric ([double[]]$rtfRatios)
    $performanceVerdict = if (@($ipsMetric.verdict, $rtfMetric.verdict) -contains "regression") {
        "regression"
    } elseif (@($ipsMetric.verdict, $rtfMetric.verdict) -contains "inconclusive") {
        "inconclusive"
    } else {
        "pass"
    }
    $performanceReasons = @()
    if ($ipsMetric.verdict -ne "pass") {
        $performanceReasons += "instructions-per-host-second is $($ipsMetric.verdict)"
    }
    if ($rtfMetric.verdict -ne "pass") {
        $performanceReasons += "real-time factor is $($rtfMetric.verdict)"
    }

    return [ordered]@{
        name = $Policy.name
        mode = $Policy.mode
        configuration = [ordered]@{
            cpu_mode = $Policy.mode
            memory_mib = 24
            video = "vega"
            cycle_budget = $Policy.cycle_budget
            cycle_budget_policy = "safety ceiling; semantic completion and TestExit code 0 are required"
        }
        execution_role = $ExecutionPolicy.name
        candidate = Get-RoleSummary "candidate" $Policy.mode $Candidate $false
        parent = Get-RoleSummary "parent" $Policy.mode $Parent $false
        discarded_warmups = [ordered]@{
            candidate = [object[]]$candidateWarmups
            parent = [object[]]$parentWarmups
            included_in_performance_statistics = $false
        }
        pairs = $pairs
        paired_metrics = [ordered]@{
            instructions_per_host_second = $ipsMetric
            real_time_factor = $rtfMetric
        }
        exact_work = [ordered]@{
            verdict = if ($exactWorkReasons.Count -eq 0) { "pass" } else { "fail" }
            warmup = $warmupComparison
            candidate_role_determinism = $candidateDeterminism
            parent_role_determinism = $parentDeterminism
            failure_reasons = [object[]]$exactWorkReasons
        }
        checks = [ordered]@{
            semantic = [ordered]@{
                verdict = if ($semanticReasons.Count -eq 0) { "pass" } else { "fail" }
                failure_reasons = [object[]]$semanticReasons
            }
            provenance = [ordered]@{
                verdict = if ($provenanceReasons.Count -eq 0) { "pass" } else { "fail" }
                failure_reasons = [object[]]$provenanceReasons
            }
            performance = [ordered]@{
                verdict = $performanceVerdict
                failure_reasons = [object[]]$performanceReasons
            }
        }
        verdicts = [ordered]@{
            semantic = if ($semanticReasons.Count -eq 0) { "pass" } else { "fail" }
            exact_work = if ($exactWorkReasons.Count -eq 0) { "pass" } else { "fail" }
            provenance = if ($provenanceReasons.Count -eq 0) { "pass" } else { "fail" }
            performance = $performanceVerdict
        }
        failure_reasons = [object[]]@(
            @($semanticReasons) + @($exactWorkReasons) +
            @($provenanceReasons) + @($performanceReasons)
        )
    }
}

function New-TrackMComparisonSummary([object[]]$Workloads) {
    $aggregateVerdicts = [ordered]@{}
    foreach ($component in @("semantic", "exact_work", "provenance")) {
        $aggregateVerdicts[$component] = if (@($Workloads | Where-Object {
            $_.verdicts.$component -ne "pass"
        }).Count -eq 0) { "pass" } else { "fail" }
    }
    $performanceValues = @($Workloads.verdicts.performance)
    $aggregateVerdicts.performance = if ($performanceValues -contains "regression") {
        "regression"
    } elseif ($performanceValues -contains "inconclusive") {
        "inconclusive"
    } else {
        "pass"
    }

    $globalProvenanceReasons = @()
    if (-not $candidateArtifact.verified -or -not $candidateArtifact.built_this_invocation -or
        -not $baselineArtifact.verified -or -not $baselineArtifact.built_this_invocation) {
        $globalProvenanceReasons += "candidate and parent were not freshly built from isolated revisions"
    }
    if ($candidateArtifact.artifact_source.head_commit -cne $revision -or
        $candidateArtifact.artifact_source.head_tree -cne $repositoryAtSelection.head_tree -or
        $baselineArtifact.artifact_source.head_commit -cne $baselineCommit -or
        $baselineArtifact.artifact_source.head_tree -cne $baselineTree) {
        $globalProvenanceReasons += "candidate or parent artifact revision identity is wrong"
    }
    if ($candidateArtifact.build.recipe_fingerprint_sha256 -cne
        $baselineArtifact.build.recipe_fingerprint_sha256) {
        $globalProvenanceReasons += "candidate and parent build recipes differ"
    }
    if ($repositoryAtSelection.dirty -or -not $repositoryStable) {
        $globalProvenanceReasons += "the candidate repository was dirty or changed during measurement"
    }
    if (-not $candidateExecutableStable -or -not $parentExecutableStable) {
        $globalProvenanceReasons += "a frozen revision executable changed during measurement"
    }
    if (-not $doomFrozenStable -or -not $quakeFrozenStable -or
        -not $doomSourceStable -or -not $quakeSourceStable) {
        $globalProvenanceReasons += "a source or frozen workload tree changed during measurement"
    }
    if (-not $gateSourceClosureStable -or $gateScriptHashAfter -cne $gateScriptHash) {
        $globalProvenanceReasons += "the gate source closure changed during measurement"
    }
    if (-not $fixtureManifestStable) {
        $globalProvenanceReasons += "the accepted workload manifest changed during measurement"
    }
    if (-not $verifiedChildAffinityStable -or
        $verifiedChildAffinityMasks.Count -ne $policies.Count * (2 + 2 * $Runs)) {
        $globalProvenanceReasons += "not every Track M child used verified processor 8 affinity"
    }
    if ($null -ne $outerAffinityRestoreFailure) {
        $globalProvenanceReasons += "the gate process affinity did not restore after measurement"
    }
    if (-not $powerSchemeStable) {
        $globalProvenanceReasons += if ($powerSchemeRecorded) {
            "the active power scheme changed during measurement"
        } else {
            "the active power scheme could not be recorded"
        }
    }
    if ($null -eq $measurementLockEvidence -or
        $measurementLockEvidence.path -cne [IO.Path]::GetFullPath($MeasurementLockPath)) {
        $globalProvenanceReasons += "the exclusive measurement lock is missing or wrong"
    }
    if ($detectedBuildEnvironmentOverrides.Count -ne 0) {
        $globalProvenanceReasons += "build environment overrides were present"
    }
    if ($Runs -notin @(3, 6) -or $Workloads.Count -ne 3) {
        $globalProvenanceReasons += "the Track M workload or pair count is incomplete"
    }
    if ($globalProvenanceReasons.Count -ne 0) {
        $aggregateVerdicts.provenance = "fail"
    }

    $failureReasons = @()
    foreach ($workload in $Workloads) {
        foreach ($reason in @($workload.failure_reasons)) {
            $failureReasons += "$($workload.name): $reason"
        }
    }
    $failureReasons += $globalProvenanceReasons
    $nonPerformancePass = $aggregateVerdicts.semantic -eq "pass" -and
        $aggregateVerdicts.exact_work -eq "pass" -and
        $aggregateVerdicts.provenance -eq "pass"
    $verdict = if (-not $nonPerformancePass -or
        $aggregateVerdicts.performance -eq "regression") {
        "failed"
    } elseif ($aggregateVerdicts.performance -eq "inconclusive") {
        "inconclusive"
    } else {
        "passed"
    }
    $retentionEligible = $verdict -eq "passed"
    $sixPairRerunEligible = [bool]($Screening -and $nonPerformancePass -and
        $aggregateVerdicts.performance -eq "inconclusive")
    $quakeOverrides = Get-BackendQuakeCompletionOverrides

    return [ordered]@{
        schema = "izarravm-track-m-revision-pair-v1"
        comparison_class = "immediate_parent_revision_pair"
        formal = $true
        evidence_grade = if ($Screening) { "three_pair_screen" } else { "six_pair_confirmation" }
        verdict = $verdict
        retention_eligible = $retentionEligible
        six_pair_rerun_eligible = $sixPairRerunEligible
        verdicts = $aggregateVerdicts
        revision_pair = [ordered]@{
            candidate_commit = $revision
            candidate_tree = $candidateArtifact.artifact_source.head_tree
            parent_commit = $baselineCommit
            parent_tree = $baselineTree
            derivation = "unique immediate parent of candidate commit"
        }
        executables = [ordered]@{
            candidate = $candidateArtifact
            parent = $baselineArtifact
            same_isolated_build_recipe = $candidateArtifact.build.recipe_fingerprint_sha256 -ceq
                $baselineArtifact.build.recipe_fingerprint_sha256
        }
        execution = [ordered]@{
            role = $trackMExecutionPolicy.name
            candidate = [ordered]@{
                cli = $trackMExecutionPolicy.cli
                environment = [ordered]@{ IZARRAVM_JIT = $trackMExecutionPolicy.jit }
            }
            parent = [ordered]@{
                cli = $trackMExecutionPolicy.cli
                environment = [ordered]@{ IZARRAVM_JIT = $trackMExecutionPolicy.jit }
            }
        }
        repository_at_selection = $repositoryAtSelection
        repository_at_completion = $repositoryAtCompletion
        verification = [ordered]@{
            build_environment_override_names = @($detectedBuildEnvironmentOverrides.Keys | Sort-Object)
            workload_manifest_matches = $fixtureManifestMatches
            source_and_frozen_workload_trees_stable = [ordered]@{
                doom_source = $doomSourceStable
                doom_frozen = $doomFrozenStable
                quake_source = $quakeSourceStable
                quake_frozen = $quakeFrozenStable
            }
        }
        measurement_lock = $measurementLockEvidence
        gate_source_closure = $gateSourceClosureEvidence
        workload_manifest_sha256 = [ordered]@{
            at_entry = $fixtureManifestHash
            at_completion = $fixtureManifestHashAfter
        }
        workload_inputs_sha256 = $workloadInputHashes
        workload_trees_sha256 = $workloadTreeHashes
        workload_canonical_trees_sha256 = $workloadCanonicalTreeHashes
        injected_exitvm_sha256 = $exitVmHash
        quake_completion_fixture = [ordered]@{
            fixture_class = "track_m_quake_semantic_completion_v1"
            fresh_disposable_copy_per_observation = $true
            command_sequence = "quake.exe -nosound -nocdaudio -nojoy -condebug +timedemo demo1 +startdemos +exec bench.cfg"
            autoexec_override_sha256 = $quakeOverrides.autoexec_sha256
            bench_cfg_override_sha256 = $quakeOverrides.bench_cfg_sha256
            wait_marker = $quakeOverrides.wait_marker
            dos_return_proof = "the hashed AUTOEXEC override runs EXITVM.COM immediately after Quake returns"
            required_stop = "Lotura TestExit code 0"
            cycle_budget_policy = "safety ceiling only"
        }
        generated_utc = [DateTime]::UtcNow.ToString("o")
        host = [ordered]@{
            identity = $hostIdentity
            active_power_scheme_at_completion = $activePowerSchemeAtCompletion
            active_power_scheme_recorded = $powerSchemeRecorded
            active_power_scheme_stable = $powerSchemeStable
        }
        processor_affinity = [ordered]@{
            policy = "one inherited processor per child"
            requested_processor_index = $ProcessorIndex
            requested_mask = Format-AffinityMask $requestedProcessorMask
            verified_child_processes = $verifiedChildAffinityMasks.Count
            expected_child_processes = $policies.Count * (2 + 2 * $Runs)
            verified_child_masks = @($verifiedChildAffinityMasks | Sort-Object -Unique)
            parent_restore_succeeded = $null -eq $outerAffinityRestoreFailure
        }
        measured_pairs_per_workload = $Runs
        discarded_warmups_per_role_and_workload = 1
        pair_seed = $PairSeed
        warmup_order = @(Get-PairOrder 1 $PairSeed $pairRoles)
        pair_order = @(1..$Runs | ForEach-Object {
            [ordered]@{ pair = $_; roles = @(Get-PairOrder $_ $PairSeed $pairRoles) }
        })
        wall_samples_serialized = $true
        acceptance = [ordered]@{
            minimum_median_ratio = 0.99
            minimum_lower_95_ratio = 0.97
            paired_lower_bound = "one-sided 95% Student-t"
            exact_work_fields = @(
                "perf.instructions", "master_ticks", "elapsed_budget_clocks",
                "executed_cpu_core_clocks", "raw_bus_clocks", "stop",
                "timedemo_identity", "result_block_identity",
                "measurement_fixture_identity", "quake_completion_identity",
                "qconsole_sha256"
            )
            warmups_are_discarded_from_statistics = $true
            favorable_pair_selection = "forbidden"
        }
        workloads = $Workloads
        failure_reasons = [object[]]$failureReasons
    }
}

function Add-TrackMExpectedResultArtifact(
    [Collections.Generic.Dictionary[string, object]]$Expected,
    [Collections.Generic.List[string]]$Failures,
    [string]$FileName,
    [string]$ExpectedSha256,
    [string]$ArtifactClass,
    [string]$Context
) {
    if ([string]::IsNullOrWhiteSpace($FileName) -or
        [IO.Path]::GetFileName($FileName) -cne $FileName) {
        $Failures.Add("$Context has an invalid artifact file name")
        return
    }
    if ($ExpectedSha256 -notmatch '^[0-9a-f]{64}$') {
        $Failures.Add("$Context has an invalid recorded SHA-256")
        return
    }
    if ($Expected.ContainsKey($FileName)) {
        $Failures.Add("$Context duplicates result artifact $FileName")
        return
    }
    $Expected.Add($FileName, [pscustomobject][ordered]@{
        expected_sha256 = $ExpectedSha256
        artifact_class = $ArtifactClass
        context = $Context
    })
}

function Get-TrackMResultFileRecord(
    [string]$Root,
    [IO.FileInfo]$File,
    $ExpectedEntry
) {
    $relative = [IO.Path]::GetRelativePath($Root, $File.FullName).Replace('\', '/')
    if ($relative.Contains("..") -or $relative.Contains([char]0) -or
        $relative.Contains("`n") -or $relative.Contains("`r")) {
        throw "A Track M result artifact escaped its evidence directory."
    }
    return [pscustomobject][ordered]@{
        path = $relative
        artifact_class = if ($null -ne $ExpectedEntry) {
            $ExpectedEntry.artifact_class
        } else {
            "unexpected"
        }
        byte_length = $File.Length
        sha256 = Get-FileSha256 $File.FullName
        expected_sha256 = if ($null -ne $ExpectedEntry) {
            $ExpectedEntry.expected_sha256
        } else {
            $null
        }
    }
}

function Get-TrackMEvidenceFinalVerificationFailures(
    [string]$ResultsRoot,
    [object[]]$ResultRecords,
    [object[]]$SourceRecords,
    [Collections.IDictionary]$SourcePaths,
    $FixtureRecord,
    [string]$FixtureManifestPath,
    [string]$ManifestPath,
    [string]$ManifestSha256,
    [string]$ResultLogPath,
    [string]$ResultLogSha256
) {
    $failures = @()
    $root = [IO.Path]::GetFullPath($ResultsRoot)
    $rootPrefix = $root.TrimEnd([IO.Path]::DirectorySeparatorChar) +
        [IO.Path]::DirectorySeparatorChar
    $expectedPaths = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    foreach ($record in $ResultRecords) {
        $null = $expectedPaths.Add([string]$record.path)
        $path = [IO.Path]::GetFullPath((Join-Path $root ([string]$record.path)))
        if (-not $path.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase) -or
            -not (Test-Path -LiteralPath $path -PathType Leaf)) {
            $failures += "final result artifact $($record.path) is missing or escaped the evidence directory"
            continue
        }
        $file = Get-Item -LiteralPath $path -Force
        if ($file.Length -ne $record.byte_length -or
            (Get-FileSha256 $path) -cne $record.sha256) {
            $failures += "final result artifact $($record.path) changed after manifest capture"
        }
    }
    $null = $expectedPaths.Add([IO.Path]::GetFileName($ManifestPath))
    $null = $expectedPaths.Add([IO.Path]::GetFileName($ResultLogPath))
    $actualPaths = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    foreach ($file in @(Get-ChildItem -LiteralPath $root -File -Recurse -Force)) {
        $relative = [IO.Path]::GetRelativePath($root, $file.FullName).Replace('\', '/')
        $null = $actualPaths.Add($relative)
        if (-not $expectedPaths.Contains($relative)) {
            $failures += "unexpected final evidence file $relative"
        }
    }
    foreach ($path in $expectedPaths) {
        if (-not $actualPaths.Contains($path)) {
            $failures += "missing final evidence file $path"
        }
    }
    foreach ($record in $SourceRecords) {
        $path = [string]$SourcePaths[[string]$record.path]
        if ([string]::IsNullOrWhiteSpace($path) -or
            -not (Test-Path -LiteralPath $path -PathType Leaf)) {
            $failures += "gate source member $($record.path) is missing during final verification"
            continue
        }
        $file = Get-Item -LiteralPath $path
        if ($file.Length -ne $record.byte_length -or
            (Get-FileSha256 $path) -cne $record.sha256) {
            $failures += "gate source member $($record.path) changed after manifest capture"
        }
    }
    if (-not (Test-Path -LiteralPath $FixtureManifestPath -PathType Leaf)) {
        $failures += "the workload manifest is missing during final verification"
    } else {
        $fixtureFile = Get-Item -LiteralPath $FixtureManifestPath
        if ($fixtureFile.Length -ne $FixtureRecord.byte_length -or
            (Get-FileSha256 $fixtureFile.FullName) -cne $FixtureRecord.sha256) {
            $failures += "the workload manifest changed after manifest capture"
        }
    }
    if (-not (Test-Path -LiteralPath $ManifestPath -PathType Leaf) -or
        (Get-FileSha256 $ManifestPath) -cne $ManifestSha256) {
        $failures += "the evidence manifest changed after its hash was captured"
    }
    if (-not (Test-Path -LiteralPath $ResultLogPath -PathType Leaf) -or
        (Get-FileSha256 $ResultLogPath) -cne $ResultLogSha256) {
        $failures += "result.log changed after it was written"
    }
    return [object[]]$failures
}

function Write-TrackMEvidencePackage(
    [string]$ResultsRoot,
    [string]$SummaryPath,
    $Summary,
    $CandidateArtifact,
    $ParentArtifact,
    $GateSourceClosure,
    [string]$MainPath,
    [string]$SelfTestPath,
    [string]$SummaryScriptPath,
    [string]$FixtureManifestPath
) {
    $root = [IO.Path]::GetFullPath($ResultsRoot)
    $summaryFullPath = [IO.Path]::GetFullPath($SummaryPath)
    $rootPrefix = $root.TrimEnd([IO.Path]::DirectorySeparatorChar) +
        [IO.Path]::DirectorySeparatorChar
    if (-not $summaryFullPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase) -or
        -not (Test-Path -LiteralPath $summaryFullPath -PathType Leaf)) {
        throw "Track M summary is outside the evidence directory or missing."
    }
    $manifestPath = Join-Path $root "evidence-manifest.json"
    $resultLogPath = Join-Path $root "result.log"
    if ((Test-Path -LiteralPath $manifestPath) -or
        (Test-Path -LiteralPath $resultLogPath)) {
        throw "Track M evidence manifest and result log must be written into a new directory."
    }

    $failures = [Collections.Generic.List[string]]::new()
    $expected = [Collections.Generic.Dictionary[string, object]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    Add-TrackMExpectedResultArtifact $expected $failures `
        ([IO.Path]::GetFileName($CandidateArtifact.executed_copy_path)) `
        $CandidateArtifact.sha256 "candidate_executable" "candidate executable"
    Add-TrackMExpectedResultArtifact $expected $failures `
        ([IO.Path]::GetFileName($ParentArtifact.executed_copy_path)) `
        $ParentArtifact.sha256 "parent_executable" "parent executable"
    $summaryHash = Get-FileSha256 $summaryFullPath
    Add-TrackMExpectedResultArtifact $expected $failures `
        ([IO.Path]::GetFileName($summaryFullPath)) $summaryHash "summary" "final summary"

    foreach ($workload in @($Summary.workloads)) {
        foreach ($role in @("candidate", "parent")) {
            $samples = @($workload.discarded_warmups.$role) + @($workload.$role.runs)
            foreach ($sample in $samples) {
                $context = "$($workload.name) $role $($sample.gate_observation)"
                $artifacts = $sample.gate_artifacts
                foreach ($name in @("profile_json", "stdout", "stderr")) {
                    $fileProperty = "${name}_file"
                    $hashProperty = "${name}_sha256"
                    Add-TrackMExpectedResultArtifact $expected $failures `
                        ([string]$artifacts.$fileProperty) ([string]$artifacts.$hashProperty) `
                        $name "$context $name"
                }
                if ($workload.name -ceq "quake-586") {
                    Add-TrackMExpectedResultArtifact $expected $failures `
                        ([string]$artifacts.qconsole_file) ([string]$artifacts.qconsole_sha256) `
                        "qconsole" "$context qconsole"
                } elseif ($null -ne $artifacts.qconsole_file -or
                    $null -ne $artifacts.qconsole_sha256) {
                    $failures.Add("$context has non-null QCONSOLE evidence")
                }
            }
        }
    }

    $resultRecords = @()
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($file in @(Get-ChildItem -LiteralPath $root -File -Recurse -Force |
        Sort-Object FullName)) {
        $relativeName = [IO.Path]::GetRelativePath($root, $file.FullName).Replace('\', '/')
        $entry = if ($expected.ContainsKey($relativeName)) {
            $expected[$relativeName]
        } else {
            $null
        }
        $record = Get-TrackMResultFileRecord $root $file $entry
        $resultRecords += $record
        $null = $seen.Add($relativeName)
        if ($null -eq $entry) {
            $failures.Add("unexpected result artifact $relativeName")
        } elseif ($record.sha256 -cne $entry.expected_sha256) {
            $failures.Add("$($entry.context) SHA-256 does not match its recorded value")
        }
    }
    foreach ($entry in $expected.GetEnumerator()) {
        if (-not $seen.Contains($entry.Key)) {
            $failures.Add("missing result artifact $($entry.Key)")
        }
    }

    $sourcePaths = [ordered]@{
        "scripts/run-realtime-gate.ps1" = $MainPath
        "scripts/run-realtime-gate-self-test.ps1" = $SelfTestPath
        "scripts/run-realtime-gate-summary.ps1" = $SummaryScriptPath
    }
    $sourceRecords = @()
    foreach ($source in $sourcePaths.GetEnumerator()) {
        $identity = Get-GateSourceMemberIdentity $source.Key $source.Value
        $recorded = @($GateSourceClosure.members | Where-Object { $_.label -ceq $source.Key })
        if ($recorded.Count -ne 1 -or $recorded[0].byte_length -ne $identity.byte_length -or
            $recorded[0].sha256 -cne $identity.sha256) {
            $failures.Add("gate source member $($source.Key) does not match the closed source identity")
        }
        $sourceRecords += [pscustomobject][ordered]@{
            path = $source.Key
            artifact_class = "gate_source"
            byte_length = $identity.byte_length
            sha256 = $identity.sha256
        }
    }
    $fixtureFile = Get-Item -LiteralPath $FixtureManifestPath
    $fixtureRecord = [pscustomobject][ordered]@{
        path = "scripts/realtime-gate-inputs.json"
        artifact_class = "workload_manifest"
        byte_length = $fixtureFile.Length
        sha256 = Get-FileSha256 $fixtureFile.FullName
    }
    if ($fixtureRecord.sha256 -cne $Summary.workload_manifest_sha256.at_completion) {
        $failures.Add("workload manifest SHA-256 does not match the final summary")
    }

    $resultRecords = @($resultRecords | Sort-Object path)
    $sourceRecords = @($sourceRecords | Sort-Object path)
    $manifest = [ordered]@{
        schema = "izarravm-track-m-evidence-manifest-v1"
        comparison_schema = $Summary.schema
        execution_role = $Summary.execution.role
        verdict = $Summary.verdict
        revision_pair = $Summary.revision_pair
        summary = [ordered]@{
            path = [IO.Path]::GetFileName($summaryFullPath)
            byte_length = (Get-Item -LiteralPath $summaryFullPath).Length
            sha256 = $summaryHash
        }
        result_directory_files = [object[]]$resultRecords
        gate_source_members = [object[]]$sourceRecords
        workload_manifest = $fixtureRecord
        coverage = [ordered]@{
            expected_result_files = $expected.Count
            observed_result_files = $resultRecords.Count
            all_expected_present = @($expected.Keys | Where-Object { -not $seen.Contains($_) }).Count -eq 0
            no_unexpected_files = @($resultRecords | Where-Object {
                $_.artifact_class -ceq "unexpected"
            }).Count -eq 0
        }
        integrity_verdict = if ($failures.Count -eq 0) { "pass" } else { "fail" }
        integrity_failures = [object[]]@($failures)
    }
    $manifest | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $manifestPath -Encoding utf8
    $manifestHash = Get-FileSha256 $manifestPath
    $logLines = @(
        "schema=izarravm-track-m-result-v1",
        "verdict=$($Summary.verdict)",
        "retention_eligible=$($Summary.retention_eligible.ToString().ToLowerInvariant())",
        "execution_role=$($Summary.execution.role)",
        "candidate_commit=$($Summary.revision_pair.candidate_commit)",
        "candidate_tree=$($Summary.revision_pair.candidate_tree)",
        "parent_commit=$($Summary.revision_pair.parent_commit)",
        "parent_tree=$($Summary.revision_pair.parent_tree)",
        "summary_sha256=$summaryHash",
        "evidence_manifest_sha256=$manifestHash",
        "evidence_integrity=$($manifest.integrity_verdict)"
    )
    [IO.File]::WriteAllText(
        $resultLogPath,
        ($logLines -join "`n") + "`n",
        [Text.UTF8Encoding]::new($false)
    )
    $resultLogHash = Get-FileSha256 $resultLogPath
    $finalFailures = @(Get-TrackMEvidenceFinalVerificationFailures `
        $root $resultRecords $sourceRecords $sourcePaths $fixtureRecord `
        $FixtureManifestPath $manifestPath $manifestHash $resultLogPath $resultLogHash)
    if ($finalFailures.Count -ne 0) {
        throw "Track M evidence changed during final verification: $($finalFailures -join '; ')"
    }
    if ($failures.Count -ne 0) {
        throw "Track M evidence integrity failed after packaging: $($failures -join '; ')"
    }
    return [pscustomobject][ordered]@{
        manifest_path = $manifestPath
        manifest_sha256 = $manifestHash
        result_log_path = $resultLogPath
        summary_sha256 = $summaryHash
    }
}
