# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

function Invoke-RealtimeGateSelfTest {
    $identity = ConvertFrom-QuakeTimedemoLine "969 frames  22.8 seconds  42.6 fps"
    if ($null -eq $identity -or $identity.frames -ne 969 -or
        $identity.seconds -ne 22.8 -or $identity.fps -ne 42.6) {
        throw "The Quake timedemo parser rejected a valid identity."
    }
    if ($null -ne (ConvertFrom-QuakeTimedemoLine "969 frames, 22.8 seconds")) {
        throw "The Quake timedemo parser accepted an invalid identity."
    }
    Assert-QuakeAutoexecText "quake.exe -nosound +timedemo demo1"
    Assert-SelfTestThrows {
        Assert-QuakeAutoexecText "quake.exe -nosound +timedemo demo1 +exec bench.cfg"
    } "must not execute bench.cfg"
    Assert-SelfTestThrows {
        Assert-QuakeAutoexecText "quake.exe -nosound"
    } "must launch +timedemo demo1"
    if (-not (Test-BackendQuakeCompletionOverride $true "quake-586") -or
        (Test-BackendQuakeCompletionOverride $false "quake-586") -or
        (Test-BackendQuakeCompletionOverride $true "doom-586") -or
        (Test-BackendQuakeCompletionOverride $false "doom-586")) {
        throw "The BackendBakeoff Quake completion override selection leaked into another policy."
    }
    if (-not (Test-ObservationRequiresTestExit $true "quake-586") -or
        (Test-ObservationRequiresTestExit $false "quake-586") -or
        -not (Test-ObservationRequiresTestExit $true "doom-586") -or
        -not (Test-ObservationRequiresTestExit $false "doom-586")) {
        throw "The observation TestExit selection changed a normal or BackendBakeoff policy."
    }
    $completionOverrides = Get-BackendQuakeCompletionOverrides
    if ($completionOverrides.autoexec_bytes.Length -ne 125 -or
        $completionOverrides.autoexec_sha256 -cne $backendQuakeAutoexecSha256 -or
        $completionOverrides.bench_cfg_bytes.Length -ne 251 -or
        $completionOverrides.bench_cfg_sha256 -cne $backendQuakeBenchCfgSha256 -or
        [Text.Encoding]::ASCII.GetString($completionOverrides.autoexec_bytes) -notmatch
            '\+timedemo demo1 \+startdemos \+exec bench\.cfg') {
        throw "The BackendBakeoff Quake completion override identity is wrong."
    }
    $quakeCompletionSelfTestRoot = Join-Path ([IO.Path]::GetTempPath()) (
        "izarravm-quake-completion-$([guid]::NewGuid().ToString('N'))"
    )
    New-Item -ItemType Directory -Path $quakeCompletionSelfTestRoot | Out-Null
    $quakeCompletionLog = Join-Path $quakeCompletionSelfTestRoot "QCONSOLE.LOG"
    $quakeCompletionStdout = Join-Path $quakeCompletionSelfTestRoot "stdout.log"
    $quakeCompletionStderr = Join-Path $quakeCompletionSelfTestRoot "stderr.log"
    try {
        $newCompletionFixture = {
            param(
                [string]$Name,
                [bool]$OmitBenchCfg = $false,
                [bool]$UseStaleQconsoleDirectory = $false
            )
            $fixtureRoot = Join-Path $quakeCompletionSelfTestRoot $Name
            $id1 = Join-Path $fixtureRoot "QUAKE/ID1"
            New-Item -ItemType Directory -Path $id1 | Out-Null
            [IO.File]::WriteAllText(
                (Join-Path $fixtureRoot "AUTOEXEC.BAT"),
                "canonical autoexec`n",
                [Text.Encoding]::ASCII
            )
            [IO.File]::WriteAllText(
                (Join-Path $fixtureRoot "CONFIG.SYS"),
                "canonical config`n",
                [Text.Encoding]::ASCII
            )
            if (-not $OmitBenchCfg) {
                [IO.File]::WriteAllText(
                    (Join-Path $id1 "bench.cfg"),
                    "canonical bench`n",
                    [Text.Encoding]::ASCII
                )
            }
            $staleQconsole = Join-Path $id1 "QCONSOLE.LOG"
            if ($UseStaleQconsoleDirectory) {
                New-Item -ItemType Directory -Path $staleQconsole | Out-Null
            } else {
                [IO.File]::WriteAllText(
                    $staleQconsole,
                    "stale console`n",
                    [Text.Encoding]::ASCII
                )
            }
            return $fixtureRoot
        }
        $fixtureExitBytes = [Text.Encoding]::ASCII.GetBytes("exit fixture`n")
        $fixtureExitHash = Get-BytesSha256 $fixtureExitBytes
        $successfulFixture = & $newCompletionFixture "fixture-success"
        $successfulCanonicalHash = Get-DirectoryTreeSha256 $successfulFixture @(
            "EXITVM.COM", "QUAKE/ID1/QCONSOLE.LOG"
        )
        $fixtureEvidence = Set-BackendQuakeCompletionFixture `
            $successfulFixture $successfulCanonicalHash $fixtureExitBytes $fixtureExitHash
        if ($fixtureEvidence.canonical_tree_sha256 -cne $successfulCanonicalHash -or
            $fixtureEvidence.autoexec_override_sha256 -cne $backendQuakeAutoexecSha256 -or
            $fixtureEvidence.bench_cfg_override_sha256 -cne $backendQuakeBenchCfgSha256 -or
            $fixtureEvidence.exitvm_sha256 -cne $fixtureExitHash -or
            $fixtureEvidence.prelaunch_overridden_tree_sha256 -cne (
                Get-DirectoryTreeSha256 $successfulFixture @("QUAKE/ID1/QCONSOLE.LOG")
            ) -or
            -not $fixtureEvidence.stale_qconsole_absent_before_launch -or
            (Test-Path -LiteralPath (Join-Path $successfulFixture "QUAKE/ID1/QCONSOLE.LOG"))) {
            throw "The production Quake completion fixture writer returned invalid evidence."
        }
        $staleDirectoryFixture = & $newCompletionFixture "fixture-stale-directory" $false $true
        $staleDirectoryCanonicalHash = Get-DirectoryTreeSha256 $staleDirectoryFixture @(
            "EXITVM.COM", "QUAKE/ID1/QCONSOLE.LOG"
        )
        Assert-SelfTestThrows {
            Set-BackendQuakeCompletionFixture `
                $staleDirectoryFixture $staleDirectoryCanonicalHash $fixtureExitBytes $fixtureExitHash
        } "stale QCONSOLE.LOG"
        $wrongCanonicalFixture = & $newCompletionFixture "fixture-wrong-canonical"
        Assert-SelfTestThrows {
            Set-BackendQuakeCompletionFixture `
                $wrongCanonicalFixture ("0" * 64) $fixtureExitBytes $fixtureExitHash
        } "verified canonical tree"
        $missingTargetFixture = & $newCompletionFixture "fixture-missing-target" $true
        $missingTargetCanonicalHash = Get-DirectoryTreeSha256 $missingTargetFixture @(
            "EXITVM.COM", "QUAKE/ID1/QCONSOLE.LOG"
        )
        Assert-SelfTestThrows {
            Set-BackendQuakeCompletionFixture `
                $missingTargetFixture $missingTargetCanonicalHash $fixtureExitBytes $fixtureExitHash
        } "override target is missing"
        $wrongExitFixture = & $newCompletionFixture "fixture-wrong-exit"
        $wrongExitCanonicalHash = Get-DirectoryTreeSha256 $wrongExitFixture @(
            "EXITVM.COM", "QUAKE/ID1/QCONSOLE.LOG"
        )
        Assert-SelfTestThrows {
            Set-BackendQuakeCompletionFixture `
                $wrongExitFixture $wrongExitCanonicalHash $fixtureExitBytes ("0" * 64)
        } "prelaunch bytes"
        [IO.File]::WriteAllText($quakeCompletionStdout, "", [Text.Encoding]::ASCII)
        [IO.File]::WriteAllText($quakeCompletionStderr, "", [Text.Encoding]::ASCII)
        $validCompletionText = "969 frames  22.8 seconds  42.5 fps`n$backendQuakeWaitMarker`n"
        [IO.File]::WriteAllText($quakeCompletionLog, $validCompletionText, [Text.Encoding]::ASCII)
        $validCompletion = Read-BackendQuakeCompletion `
            $quakeCompletionLog @($quakeCompletionStdout, $quakeCompletionStderr)
        if (@(Get-BackendQuakeCompletionReasons $validCompletion "valid").Count -ne 0) {
            throw "The ordered Quake completion parser rejected valid evidence."
        }
        $invalidCompletionCases = [ordered]@{
            missing_marker = "969 frames  22.8 seconds  42.5 fps`n"
            duplicate_marker = "969 frames  22.8 seconds  42.5 fps`n$backendQuakeWaitMarker`n$backendQuakeWaitMarker`n"
            result_after_marker = "$backendQuakeWaitMarker`n969 frames  22.8 seconds  42.5 fps`n"
            duplicate_result = "969 frames  22.8 seconds  42.5 fps`n969 frames  22.8 seconds  42.5 fps`n$backendQuakeWaitMarker`n"
        }
        foreach ($case in $invalidCompletionCases.GetEnumerator()) {
            [IO.File]::WriteAllText($quakeCompletionLog, $case.Value, [Text.Encoding]::ASCII)
            $completion = Read-BackendQuakeCompletion `
                $quakeCompletionLog @($quakeCompletionStdout, $quakeCompletionStderr)
            if (@(Get-BackendQuakeCompletionReasons $completion $case.Key).Count -eq 0) {
                throw "The Quake completion parser accepted $($case.Key)."
            }
        }
        [IO.File]::WriteAllText($quakeCompletionLog, $validCompletionText, [Text.Encoding]::ASCII)
        [IO.File]::WriteAllText(
            $quakeCompletionStdout,
            "Host_Error: synthetic fatal path`n",
            [Text.Encoding]::ASCII
        )
        $fatalCompletion = Read-BackendQuakeCompletion `
            $quakeCompletionLog @($quakeCompletionStdout, $quakeCompletionStderr)
        if ($fatalCompletion.fatal_match_count -ne 1 -or
            @(Get-BackendQuakeCompletionReasons $fatalCompletion "fatal").Count -eq 0) {
            throw "The Quake completion parser accepted fatal diagnostic text."
        }
    } finally {
        if (Test-Path -LiteralPath $quakeCompletionSelfTestRoot -PathType Container) {
            $resolvedSelfTestRoot = (Resolve-Path -LiteralPath $quakeCompletionSelfTestRoot).Path
            $resolvedHostTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd(
                [IO.Path]::DirectorySeparatorChar
            )
            if (-not $resolvedSelfTestRoot.StartsWith(
                $resolvedHostTemp + [IO.Path]::DirectorySeparatorChar,
                [StringComparison]::OrdinalIgnoreCase
            )) {
                throw "The Quake completion self-test directory escaped the host temp root."
            }
            Remove-Item -LiteralPath $resolvedSelfTestRoot -Recurse -Force
        }
    }
    Assert-UninstrumentedProfileSample ([pscustomobject]@{
        machine_phase_timing_enabled = $false
    }) "clean sample"
    Assert-SelfTestThrows {
        Assert-UninstrumentedProfileSample ([pscustomobject]@{
            machine_phase_timing_enabled = $true
        }) "instrumented sample"
    } "contaminated"
    Assert-SelfTestThrows {
        Assert-UninstrumentedProfileSample ([pscustomobject]@{}) "legacy sample"
    } "missing a boolean"
    $policies = Get-WorkloadPolicies "Both"
    if ($policies.Count -ne 3 -or ($policies.name -join ",") -ne "doom-486,doom-586,quake-586" -or
        $policies[0].minimum_real_time_factor -ne 3.5 -or
        $policies[1].minimum_real_time_factor -ne 1.4 -or
        $policies[2].minimum_real_time_factor -ne 1.4) {
        throw "Both did not expand to the three workload-specific policies."
    }
    $singlePolicySelections = [ordered]@{
        Doom = "doom-486"
        Doom586 = "doom-586"
        Quake = "quake-586"
    }
    foreach ($selection in $singlePolicySelections.GetEnumerator()) {
        $singlePolicy = @(Get-WorkloadPolicies $selection.Key)
        if ($singlePolicy.Count -ne 1 -or $singlePolicy[0].name -ne $selection.Value) {
            throw "The $($selection.Key) workload selection did not remain a one-item array."
        }
    }
    $survivalComponentSelfTest = [ordered]@{
        equal_work = "pass"
        calibration = "fail"
        backend_health = "pass"
        compatibility = "pass"
    }
    $failedSurvivalComponents = @(
        Get-FailedBackendSurvivalComponents $survivalComponentSelfTest
    )
    if ($failedSurvivalComponents.Count -ne 1 -or
        $failedSurvivalComponents[0] -ne "calibration") {
        throw "A single failed backend survival component did not remain a one-item array."
    }
    $survivalComponentSelfTest.calibration = "pass"
    $failedSurvivalComponents = @(
        Get-FailedBackendSurvivalComponents $survivalComponentSelfTest
    )
    if ($failedSurvivalComponents.Count -ne 0) {
        throw "An all-pass backend survival result did not remain an empty array."
    }
    if ((Get-PairOrder 1 0) -join "," -ne "candidate,baseline" -or
        (Get-PairOrder 2 0) -join "," -ne "baseline,candidate") {
        throw "Pair order did not alternate."
    }
    $backendRoles = @("automatic", "interpreter")
    if ((Get-PairOrder 1 0 $backendRoles) -join "," -ne "automatic,interpreter" -or
        (Get-PairOrder 2 0 $backendRoles) -join "," -ne "interpreter,automatic") {
        throw "Backend pair order did not alternate."
    }
    Assert-SelfTestThrows {
        Get-PairOrder 1 0 @("automatic", "automatic")
    } "distinct role names"
    if ((Get-PairedMetric ([double[]](1, 1, 1, 1, 1, 1))).verdict -ne "pass" -or
        (Get-PairedMetric ([double[]](0.97, 0.97, 0.97, 0.97, 0.97, 0.97))).verdict -ne "regression" -or
        (Get-PairedMetric ([double[]](0.90, 0.91, 1.00, 1.01, 1.10, 1.11))).verdict -ne "inconclusive") {
        throw "Paired metric verdict boundaries are wrong."
    }
    if ((Get-BackendPairedMetric ([double[]](1.05, 1.05, 1.05, 1.05, 1.05, 1.05))).verdict -ne "pass" -or
        (Get-BackendPairedMetric ([double[]](1.049, 1.049, 1.049, 1.049, 1.049, 1.049))).verdict -ne "fail" -or
        (Get-BackendPairedMetric ([double[]](0.9, 1.2, 0.9, 1.2, 0.9, 1.2))).verdict -ne "fail") {
        throw "Backend survival boundaries are wrong."
    }
    $expectedCriticals = [ordered]@{
        2 = 6.313751515
        3 = 2.919985580
        4 = 2.353363435
        5 = 2.131846786
        6 = 2.015048
    }
    foreach ($entry in $expectedCriticals.GetEnumerator()) {
        if ([Math]::Abs((Get-OneSided95TCritical $entry.Key) - $entry.Value) -gt 1.0e-9) {
            throw "The one-sided Student-t critical for $($entry.Key) samples is wrong."
        }
    }
    if ((Get-OneSided95TCritical 100) -ne 1.697260887) {
        throw "Large paired samples must use the conservative 30-degree critical."
    }
    $twoSampleMetric = Get-PairedMetric ([double[]](1.0, 2.0))
    $expectedTwoSampleLower = [Math]::Exp(
        [Math]::Log(2.0) / 2.0 - 6.313751515 * [Math]::Log(2.0) / 2.0
    )
    if ([Math]::Abs($twoSampleMetric.lower_95_ratio - $expectedTwoSampleLower) -gt 1.0e-12) {
        throw "The two-sample one-sided lower confidence bound is wrong."
    }
    Assert-SelfTestThrows {
        Get-OneSided95TCritical 1
    } "at least two samples"
    if ((Format-AffinityMask ([int64]1 -shl 62)) -ne "0x4000000000000000") {
        throw "Affinity masks are not serialized as fixed-width hexadecimal strings."
    }
    $environmentSelfTestName = "IZARRAVM_GATE_SELF_TEST_$([guid]::NewGuid().ToString('N'))"
    try {
        Set-GateProcessEnvironment $environmentSelfTestName "present"
        if ([Environment]::GetEnvironmentVariable($environmentSelfTestName, "Process") -ne
            "present") {
            throw "The gate process environment helper did not set a value."
        }
        Set-GateProcessEnvironment $environmentSelfTestName $null
        if (Test-Path -LiteralPath "Env:$environmentSelfTestName") {
            throw "The gate process environment helper left an empty variable behind."
        }
    } finally {
        Set-GateProcessEnvironment $environmentSelfTestName $null
    }
    if ((Get-PairedMetricVerdict 0.98 0.97) -ne "pass" -or
        (Get-PairedMetricVerdict 0.979999 0.99) -ne "regression" -or
        (Get-PairedMetricVerdict 0.98 0.969999) -ne "inconclusive") {
        throw "Exact paired metric acceptance boundaries are wrong."
    }
    Assert-SelfTestThrows {
        Get-PairedMetricVerdict ([double]::NaN) 0.97
    } "finite and positive"
    Assert-SelfTestThrows {
        Get-PairedMetric ([double[]](1, 1, 1, 1, 1, [double]::PositiveInfinity))
    } "finite and positive"
    $acceptedInput = "a" * 64
    $actualInputs = [ordered]@{ "AUTOEXEC.BAT" = $acceptedInput }
    $expectedInputs = [pscustomobject]@{ "AUTOEXEC.BAT" = $acceptedInput }
    if (-not (Assert-WorkloadInputHashes $actualInputs $expectedInputs "fixture" $true)) {
        throw "The accepted workload manifest self-test did not match."
    }
    $actualInputs["AUTOEXEC.BAT"] = "b" + $acceptedInput.Substring(1)
    Assert-SelfTestThrows {
        Assert-WorkloadInputHashes $actualInputs $expectedInputs "fixture" $true
    } "AUTOEXEC.BAT"
    Assert-SelfTestThrows {
        Assert-ExpectedSha256 ("b" + $acceptedInput.Substring(1)) $acceptedInput "fixture tree" $true
    } "accepted workload manifest"
    $boundaryPolicy = [pscustomobject]@{ minimum_real_time_factor = 1.4 }
    $boundarySamples = @(
        [pscustomobject]@{ direct_native_coverage = 0.90; direct_slow_exits_per_100_instructions = 4.999; real_time_factor = 1.4 },
        [pscustomobject]@{ direct_native_coverage = 0.91; direct_slow_exits_per_100_instructions = 4.0; real_time_factor = 1.5 },
        [pscustomobject]@{ direct_native_coverage = 0.92; direct_slow_exits_per_100_instructions = 3.0; real_time_factor = 1.6 },
        [pscustomobject]@{ direct_native_coverage = 0.93; direct_slow_exits_per_100_instructions = 2.0; real_time_factor = 1.7 },
        [pscustomobject]@{ direct_native_coverage = 0.94; direct_slow_exits_per_100_instructions = 1.0; real_time_factor = 1.3 },
        [pscustomobject]@{ direct_native_coverage = 0.95; direct_slow_exits_per_100_instructions = 0.0; real_time_factor = 1.2 }
    )
    $boundaryChecks = Get-CandidateSampleChecks $boundaryPolicy $boundarySamples
    if ($boundaryChecks.coverage_passes -ne 6 -or $boundaryChecks.exit_rate_passes -ne 6 -or
        $boundaryChecks.real_time_floor_passes -ne 4) {
        throw "Every-sample and four-of-six acceptance boundaries are wrong."
    }
    $boundarySamples[0].direct_native_coverage = 0.899999
    $boundarySamples[1].direct_slow_exits_per_100_instructions = 5.0
    $boundarySamples[3].real_time_factor = 1.3
    $boundaryChecks = Get-CandidateSampleChecks $boundaryPolicy $boundarySamples
    if ($boundaryChecks.coverage_passes -ne 5 -or $boundaryChecks.exit_rate_passes -ne 5 -or
        $boundaryChecks.real_time_floor_passes -ne 3) {
        throw "Every-sample failure and three-of-six floor boundaries are wrong."
    }
    $deterministicSamples = @(
        [pscustomobject]@{
            perf = [pscustomobject]@{ instructions = 10; jit_direct_entries = 8; jit_direct_insns = 9; jit_direct_side_exits = 1 }
            timedemo = [pscustomobject]@{ gametics = 2134; realtics = 830 }
        },
        [pscustomobject]@{
            perf = [pscustomobject]@{ instructions = 10; jit_direct_entries = 8; jit_direct_insns = 9; jit_direct_side_exits = 1 }
            timedemo = [pscustomobject]@{ gametics = 2134; realtics = 831 }
        }
    )
    Assert-SelfTestThrows {
        Assert-RoleDeterminism "doom-586" $deterministicSamples
    } "deterministic timedemo"
    Assert-FormalBaselinePolicy $false $acceptedBaselineTree $true
    Assert-SelfTestThrows {
        Assert-FormalBaselinePolicy $false ("0" * 40) $true
    } "accepted uninstrumented baseline"
    Assert-SelfTestThrows {
        Assert-FormalBaselinePolicy $false $acceptedBaselineTree $false
    } "must be an ancestor"
    Assert-NoBuildEnvironmentOverrides @{}
    Assert-SelfTestThrows {
        Assert-NoBuildEnvironmentOverrides @{ RUSTFLAGS = "-C target-cpu=native" }
    } "RUSTFLAGS"
    $recipeA = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--release", "--target-dir", "C:\scratch-a\target")
        toolchain = "rustc test"
        source_config = "none"
    }
    $recipeB = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--release", "--target-dir", "D:\scratch-b\target")
        toolchain = "rustc test"
        source_config = "none"
    }
    $recipeChanged = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--target-dir", "D:\scratch-b\target")
        toolchain = "rustc test"
        source_config = "none"
    }
    $recipeToolchainChanged = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--release", "--target-dir", "D:\scratch-b\target")
        toolchain = "rustc changed"
        source_config = "none"
    }
    $recipeConfigChanged = [ordered]@{
        recipe_id = "test"
        cargo_arguments = @("build", "--release", "--target-dir", "D:\scratch-b\target")
        toolchain = "rustc test"
        source_config = "present"
    }
    if ((Get-BuildRecipeFingerprint $recipeA) -ne (Get-BuildRecipeFingerprint $recipeB) -or
        (Get-BuildRecipeFingerprint $recipeA) -eq (Get-BuildRecipeFingerprint $recipeChanged) -or
        (Get-BuildRecipeFingerprint $recipeA) -eq (Get-BuildRecipeFingerprint $recipeToolchainChanged) -or
        (Get-BuildRecipeFingerprint $recipeA) -eq (Get-BuildRecipeFingerprint $recipeConfigChanged)) {
        throw "Build recipe fingerprint normalization is wrong."
    }
    if ((Get-ArtifactSelectionPolicy $true $true $false) -ne "unverified_prebuilt") {
        throw "Custom report-only artifacts must remain unverified."
    }
    Assert-SelfTestThrows {
        Get-ArtifactSelectionPolicy $false $true $false
    } "refuses custom"
    Assert-SelfTestThrows {
        Get-ArtifactSelectionPolicy $false $false $true
    } "refuses custom"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $true $false $false $false $false 8 "C:\gate.lock"
    } "ReportOnly"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $false $true $false $false $false 8 "C:\gate.lock"
    } "BaselineRevision"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $false $false $true $false $false 8 "C:\gate.lock"
    } "does not accept Jit"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $false $false $false $false $false 7 "C:\gate.lock"
    } "ProcessorIndex 8"
    Assert-SelfTestThrows {
        Assert-BackendBakeoffMode $false $false $false $false $false 8 "relative.lock"
    } "absolute MeasurementLockPath"
    $resultBlockSelfTestPath = Join-Path ([IO.Path]::GetTempPath()) (
        "izarravm-result-block-$([guid]::NewGuid().ToString('N')).log"
    )
    try {
        [IO.File]::WriteAllText(
            $resultBlockSelfTestPath,
            "prefix`r`n--- BEGIN RESULT ---`r`nstop: TestExit { code: 0 }`r`n--- END RESULT ---`r`nwall: 1.0s`r`n"
        )
        $resultBlock = Get-NormalizedResultBlock $resultBlockSelfTestPath
        if ($resultBlock.status -ne "valid" -or $resultBlock.block_count -ne 1 -or
            $resultBlock.normalized_bytes -le 0 -or $resultBlock.sha256.Length -ne 64) {
            throw "Normalized result-block evidence is incomplete."
        }
        [IO.File]::WriteAllText($resultBlockSelfTestPath, "no result block`n")
        $invalidResultBlock = Get-NormalizedResultBlock $resultBlockSelfTestPath
        if ($invalidResultBlock.status -ne "invalid_block_count" -or
            $invalidResultBlock.block_count -ne 0 -or $null -ne $invalidResultBlock.sha256) {
            throw "Invalid result-block evidence was not preserved as summary data."
        }
    } finally {
        Remove-Item -LiteralPath $resultBlockSelfTestPath -Force -ErrorAction SilentlyContinue
    }
    $exactSampleA = [pscustomobject]@{
        perf = [pscustomobject]@{ instructions = 10 }
        master_ticks = 20
        elapsed_budget_clocks = 30
        executed_cpu_core_clocks = 11
        raw_bus_clocks = 19
        stop = [pscustomobject]@{ kind = "test_exit"; code = 0 }
        timedemo = [pscustomobject]@{ gametics = 2134; realtics = 830 }
        gate_artifacts = [pscustomobject]@{
            result_block_status = "valid"
            result_block_sha256 = "a" * 64
        }
    }
    $exactSampleB = $exactSampleA.PSObject.Copy()
    $equalComparison = Compare-EqualWorkRecords `
        (Get-EqualWorkRecord "doom-586" $exactSampleA) `
        (Get-EqualWorkRecord "doom-586" $exactSampleB)
    if (-not $equalComparison.matches) {
        throw "Equal-work comparison rejected identical samples."
    }
    $exactSampleB.raw_bus_clocks = 18
    $unequalComparison = Compare-EqualWorkRecords `
        (Get-EqualWorkRecord "doom-586" $exactSampleA) `
        (Get-EqualWorkRecord "doom-586" $exactSampleB)
    if ($unequalComparison.matches -or
        $unequalComparison.mismatched_fields -notcontains "raw_bus_clocks") {
        throw "Equal-work comparison did not preserve a valid negative result."
    }
    $screeningPolicy = Get-BackendEvidencePolicy $true
    $finalPolicy = Get-BackendEvidencePolicy $false
    if ($screeningPolicy.final_eligible -or $screeningPolicy.measured_pairs -ne 3 -or
        -not $finalPolicy.final_eligible -or $finalPolicy.measured_pairs -ne 6) {
        throw "Backend evidence-grade eligibility is wrong."
    }
    $quakePolicy = [pscustomobject]@{
        name = "quake-586"
        cycle_budget = [uint64]6200000000
    }
    $syntheticQuakeCompletion = [pscustomobject]@{
        identity_count = 1
        timedemo = [pscustomobject]@{
            line = "969 frames  22.8 seconds  42.5 fps"
            frames = 969
            seconds = 22.8
            fps = 42.5
        }
        timedemo_line_number = 10
        wait_marker = $backendQuakeWaitMarker
        wait_marker_count = 1
        wait_marker_line_number = 11
        result_before_wait_marker = $true
        reported_values_consistent = $true
        fatal_match_count = 0
        fatal_matches = [object[]]@()
    }
    $syntheticQuakeFixture = [pscustomobject]@{
        canonical_tree_sha256 = "d" * 64
        autoexec_before_sha256 = "e" * 64
        bench_cfg_before_sha256 = "f" * 64
        autoexec_override_sha256 = $backendQuakeAutoexecSha256
        bench_cfg_override_sha256 = $backendQuakeBenchCfgSha256
        exitvm_sha256 = "a" * 64
        prelaunch_overridden_tree_sha256 = "b" * 64
        stale_qconsole_absent_before_launch = $true
    }
    $fixtureSetSamples = @(
        [pscustomobject]@{
            gate_role = "automatic"
            gate_observation = "warmup"
            gate_fixture = $syntheticQuakeFixture
        },
        [pscustomobject]@{
            gate_role = "interpreter"
            gate_observation = "pair1"
            gate_fixture = $syntheticQuakeFixture
        }
    )
    $fixtureSetIdentity = Assert-BackendQuakeFixtureSet $fixtureSetSamples
    if ([string]::IsNullOrWhiteSpace($fixtureSetIdentity)) {
        throw "The identical BackendBakeoff Quake fixture set lost its identity."
    }
    $mismatchedFixtureSample = $fixtureSetSamples[1] | ConvertTo-Json -Depth 10 | ConvertFrom-Json
    $mismatchedFixtureSample.gate_fixture.prelaunch_overridden_tree_sha256 = "c" * 64
    Assert-SelfTestThrows {
        Assert-BackendQuakeFixtureSet @($fixtureSetSamples[0], $mismatchedFixtureSample)
    } "one identical prelaunch fixture"
    $cycleLimitSamples = [ordered]@{
        automatic = @()
        interpreter = @()
    }
    foreach ($role in @("automatic", "interpreter")) {
        foreach ($pair in 1..6) {
            $cycleLimitSamples[$role] += [pscustomobject]@{
                gate_observation = "pair$pair"
                gate_process_exit_code = 0
                gate_artifacts = [pscustomobject]@{
                    result_block_status = "valid"
                    result_block_count = 1
                    result_block_sha256 = "a" * 64
                }
                stop = [pscustomobject]@{
                    kind = "cycle_limit"
                    requested = [uint64]6200000000
                }
                quake_timedemo_identity_count = 1
                quake_timedemo = [pscustomobject]@{
                    line = "969 frames  22.8 seconds  42.5 fps"
                    frames = 969
                    seconds = 22.8
                    fps = 42.5
                }
                gate_quake_completion = $syntheticQuakeCompletion
                gate_fixture = $syntheticQuakeFixture
            }
        }
    }
    $cycleProjection = Get-BackendTerminationProjection `
        $quakePolicy $cycleLimitSamples.automatic $cycleLimitSamples.interpreter
    $cycleCompatibilityReasons = @($cycleProjection.compatibility_reasons)
    $cycleTerminationReasons = @($cycleProjection.final_termination_reasons)
    $pairedMetricSentinel = [pscustomobject]@{ median_ratio = 1.2345 }
    $syntheticCycleWorkload = [pscustomobject]@{
        verdicts = [pscustomobject]@{
            compatibility = $cycleProjection.compatibility_verdict
        }
        checks = [pscustomobject]@{
            final_termination = [pscustomobject]@{
                failure_reasons = [object[]]$cycleProjection.final_termination_reasons
            }
        }
        paired_metrics = $pairedMetricSentinel
    }
    $wiredCycleTerminationReasons = @(
        Get-BackendFinalTerminationReasonsFromWorkloads @($syntheticCycleWorkload)
    )
    $cycleClassification = Get-BackendFinalClassification `
        $false $true $wiredCycleTerminationReasons.Count $true
    if ($cycleProjection.compatibility_verdict -ne "fail" -or
        $cycleCompatibilityReasons.Count -ne 12 -or
        $cycleTerminationReasons.Count -ne 12 -or
        $wiredCycleTerminationReasons.Count -ne 12 -or
        $syntheticCycleWorkload.paired_metrics.median_ratio -ne 1.2345 -or
        $cycleClassification.final_eligible -or
        $cycleClassification.track_a_survival -ne "ineligible" -or
        $cycleClassification.verdict -ne "ineligible") {
        throw "Fixed-cycle Quake evidence was not classified as diagnostic and ineligible."
    }
    $terminationCountCases = [ordered]@{
        zero = @()
        one = @("one")
        many = @("one", "two", "three")
    }
    foreach ($case in $terminationCountCases.GetEnumerator()) {
        $syntheticWorkload = [pscustomobject]@{
            checks = [pscustomobject]@{
                final_termination = [pscustomobject]@{
                    failure_reasons = [object[]]$case.Value
                }
            }
        }
        $actualReasons = @(
            Get-BackendFinalTerminationReasonsFromWorkloads @($syntheticWorkload)
        )
        if ($actualReasons.Count -ne @($case.Value).Count) {
            throw "The $($case.Key) final-termination case was scalarized."
        }
    }
    $testExitSamples = [ordered]@{
        automatic = @()
        interpreter = @()
    }
    foreach ($role in @("automatic", "interpreter")) {
        foreach ($sample in $cycleLimitSamples[$role]) {
            $copy = $sample.PSObject.Copy()
            $copy.stop = [pscustomobject]@{ kind = "test_exit"; code = 0 }
            $testExitSamples[$role] += $copy
        }
    }
    $testExitProjection = Get-BackendTerminationProjection `
        $quakePolicy $testExitSamples.automatic $testExitSamples.interpreter
    $testExitCompatibilityReasons = @($testExitProjection.compatibility_reasons)
    $testExitClassification = Get-BackendFinalClassification $false $true 0 $true
    if ($testExitProjection.compatibility_verdict -ne "pass" -or
        $testExitCompatibilityReasons.Count -ne 0 -or
        @($testExitProjection.final_termination_reasons).Count -ne 0 -or
        -not $testExitClassification.final_eligible -or
        $testExitClassification.track_a_survival -ne "pass" -or
        $testExitClassification.verdict -ne "survived") {
        throw "TestExit Quake evidence did not retain final eligibility: compatibility=$($testExitProjection.compatibility_verdict), compatibility_reasons=$($testExitCompatibilityReasons.Count), termination_reasons=$(@($testExitProjection.final_termination_reasons).Count), final=$($testExitClassification.final_eligible), survival=$($testExitClassification.track_a_survival), verdict=$($testExitClassification.verdict)."
    }
    $nonzeroExitSamples = [ordered]@{
        automatic = @($testExitSamples.automatic)
        interpreter = @($testExitSamples.interpreter)
    }
    $nonzeroExitSample = $nonzeroExitSamples.automatic[0].PSObject.Copy()
    $nonzeroExitSample.gate_process_exit_code = 9
    $nonzeroExitSamples.automatic[0] = $nonzeroExitSample
    $nonzeroExitProjection = Get-BackendTerminationProjection `
        $quakePolicy $nonzeroExitSamples.automatic $nonzeroExitSamples.interpreter
    if ($nonzeroExitProjection.compatibility_verdict -ne "fail" -or
        @($nonzeroExitProjection.compatibility_reasons).Count -ne 1 -or
        $nonzeroExitProjection.compatibility_reasons[0] -notlike "*host exit code is 9*") {
        throw "A nonzero BackendBakeoff Quake process exit was not rejected."
    }
    $warmupPackage = [ordered]@{}
    foreach ($workloadName in @("doom-486", "doom-586", "quake-586")) {
        $bucket = [ordered]@{}
        foreach ($role in @("automatic", "interpreter")) {
            $hash = if ($role -eq "automatic") { "b" * 64 } else { "c" * 64 }
            $bucket[$role] = @([pscustomobject]@{
                gate_role = $role
                gate_observation = "warmup"
                gate_process_exit_code = 0
                gate_processor_index = 8
                gate_processor_affinity_mask = "0x0000000000000100"
                gate_processor_affinity_verified = $true
                stop = [pscustomobject]@{ kind = "test_exit"; code = 0 }
                gate_quake_completion = if ($workloadName -eq "quake-586") {
                    $syntheticQuakeCompletion
                } else {
                    $null
                }
                gate_fixture = if ($workloadName -eq "quake-586") {
                    $syntheticQuakeFixture
                } else {
                    $null
                }
                gate_artifacts = [pscustomobject]@{
                    profile_json_file = "$workloadName-$role-warmup.json"
                    profile_json_sha256 = $hash
                    stdout_file = "$workloadName-$role-warmup.stdout.log"
                    stdout_sha256 = $hash
                    stderr_file = "$workloadName-$role-warmup.stderr.log"
                    stderr_sha256 = $hash
                    qconsole_file = if ($workloadName -eq "quake-586") {
                        "$workloadName-$role-warmup-qconsole.log"
                    } else {
                        $null
                    }
                    qconsole_sha256 = if ($workloadName -eq "quake-586") { $hash } else { $null }
                    result_block_status = "valid"
                    result_block_count = 1
                    result_block_sha256 = $hash
                    result_block_normalized_bytes = 128
                }
            })
        }
        $warmupPackage[$workloadName] = Get-BackendDiscardedWarmups $bucket $workloadName
    }
    $invalidResultWarmup = $warmupPackage["quake-586"].automatic[0] |
        ConvertTo-Json -Depth 10 | ConvertFrom-Json
    $invalidResultWarmup.gate_artifacts.result_block_sha256 = $null
    Assert-SelfTestThrows {
        Assert-BackendWarmupSample $invalidResultWarmup "automatic" "quake-586"
    } "valid semantic result block"
    $missingQconsoleWarmup = $warmupPackage["quake-586"].automatic[0] |
        ConvertTo-Json -Depth 10 | ConvertFrom-Json
    $missingQconsoleWarmup.gate_artifacts.qconsole_sha256 = $null
    Assert-SelfTestThrows {
        Assert-BackendWarmupSample $missingQconsoleWarmup "automatic" "quake-586"
    } "hashed console log"
    $missingMarkerWarmup = $warmupPackage["quake-586"].automatic[0] |
        ConvertTo-Json -Depth 10 | ConvertFrom-Json
    $missingMarkerWarmup.gate_quake_completion.wait_marker_count = 0
    $missingMarkerWarmup.gate_quake_completion.wait_marker_line_number = $null
    $missingMarkerWarmup.gate_quake_completion.result_before_wait_marker = $false
    Assert-SelfTestThrows {
        Assert-BackendWarmupSample $missingMarkerWarmup "automatic" "quake-586"
    } "completion protocol"
    $warmupRoundTrip = $warmupPackage | ConvertTo-Json -Depth 10 | ConvertFrom-Json
    $representedWarmups = 0
    foreach ($workloadName in @("doom-486", "doom-586", "quake-586")) {
        $roundTripBucket = $warmupRoundTrip.PSObject.Properties[$workloadName].Value
        $null = Get-BackendDiscardedWarmups $roundTripBucket $workloadName
        foreach ($role in @("automatic", "interpreter")) {
            $roleValue = $roundTripBucket.PSObject.Properties[$role].Value
            if ($roleValue -isnot [array] -or @($roleValue).Count -ne 1) {
                throw "$workloadName $role warmup did not remain a one-item JSON array."
            }
            $representedWarmups++
        }
    }
    if ($representedWarmups -ne 6) {
        throw "The warmup package did not represent all six discarded observations."
    }
    $lockSelfTestPath = Join-Path ([IO.Path]::GetTempPath()) (
        "izarravm-measurement-$([guid]::NewGuid().ToString('N')).lock"
    )
    $lockSelfTest = $null
    try {
        $lockSelfTest = Enter-MeasurementLock $lockSelfTestPath
        $lockSelfTestEvidence = Get-MeasurementLockEvidence $lockSelfTest
        Assert-SelfTestThrows {
            $secondLock = Enter-MeasurementLock $lockSelfTestPath
            $secondLock.handle.Dispose()
        } "already held or unavailable"
        $lockSelfTest.handle.Dispose()
        $lockSelfTestRaw = Get-Content -LiteralPath $lockSelfTestPath -Raw
        $expectedLockSelfTestRaw = ([ordered]@{
            pid = $lockSelfTest.pid
            acquired_utc = $lockSelfTest.acquired_utc
        } | ConvertTo-Json -Compress) + "`n"
        if ($lockSelfTestRaw -cne $expectedLockSelfTestRaw -or
            $lockSelfTestEvidence.pid -ne $lockSelfTest.pid -or
            $lockSelfTestEvidence.acquired_utc -cne $lockSelfTest.acquired_utc) {
            throw "The lock file, lease, and summary evidence do not share exact metadata."
        }
        $lockSelfTest = $null
    } finally {
        if ($null -ne $lockSelfTest) {
            $lockSelfTest.handle.Dispose()
        }
        Remove-Item -LiteralPath $lockSelfTestPath -Force -ErrorAction SilentlyContinue
    }
    $sourceClosureRoot = Join-Path ([IO.Path]::GetTempPath()) (
        "izarravm-gate-source-$([guid]::NewGuid().ToString('N'))"
    )
    [void](New-Item -ItemType Directory -Path $sourceClosureRoot)
    try {
        $sourceClosureMain = Join-Path $sourceClosureRoot "run-realtime-gate.ps1"
        $sourceClosureSelfTest = Join-Path $sourceClosureRoot `
            "run-realtime-gate-self-test.ps1"
        $sourceClosureSummary = Join-Path $sourceClosureRoot `
            "run-realtime-gate-summary.ps1"
        [IO.File]::WriteAllBytes(
            $sourceClosureMain,
            [IO.File]::ReadAllBytes($gateMainScriptPath)
        )
        [IO.File]::WriteAllBytes(
            $sourceClosureSelfTest,
            [IO.File]::ReadAllBytes($gateSelfTestScriptPath)
        )
        [IO.File]::WriteAllBytes(
            $sourceClosureSummary,
            [IO.File]::ReadAllBytes($gateSummaryScriptPath)
        )
        $sourceClosureFirst = Get-GateSourceClosureIdentity `
            $sourceClosureMain $sourceClosureSelfTest $sourceClosureSummary
        $sourceClosureSecond = Get-GateSourceClosureIdentity `
            $sourceClosureMain $sourceClosureSelfTest $sourceClosureSummary
        Assert-GateSourceClosureUnchanged `
            $sourceClosureFirst $sourceClosureSecond "during stable repeated capture"

        $expectedSourceLabels = @(
            "scripts/run-realtime-gate.ps1",
            "scripts/run-realtime-gate-self-test.ps1",
            "scripts/run-realtime-gate-summary.ps1"
        )
        $actualSourceLabels = @($sourceClosureFirst.members.label)
        if ($actualSourceLabels.Count -ne 3 -or
            ($actualSourceLabels -join "`n") -cne ($expectedSourceLabels -join "`n") -or
            @($actualSourceLabels | Sort-Object -Unique).Count -ne 3) {
            throw "The source closure did not preserve its three ordered, distinct entries."
        }
        foreach ($member in $sourceClosureFirst.members) {
            if ($member.sha256 -cnotmatch '^[0-9a-f]{64}$') {
                throw "The source closure emitted a non-canonical hash for $($member.label)."
            }
        }
        $sourceClosureRoundTrip = $sourceClosureFirst |
            ConvertTo-Json -Depth 8 | ConvertFrom-Json
        if ($sourceClosureRoundTrip.members -isnot [Array] -or
            @($sourceClosureRoundTrip.members).Count -ne 3) {
            throw "The source closure JSON round trip did not retain a three-member array."
        }

        $sourceClosurePaths = [ordered]@{
            "scripts/run-realtime-gate.ps1" = $sourceClosureMain
            "scripts/run-realtime-gate-self-test.ps1" = $sourceClosureSelfTest
            "scripts/run-realtime-gate-summary.ps1" = $sourceClosureSummary
        }
        foreach ($entry in $sourceClosurePaths.GetEnumerator()) {
            $originalBytes = [IO.File]::ReadAllBytes($entry.Value)
            $changedBytes = [byte[]]::new($originalBytes.Length + 1)
            [Array]::Copy($originalBytes, $changedBytes, $originalBytes.Length)
            $changedBytes[$changedBytes.Length - 1] = 0x0A
            try {
                [IO.File]::WriteAllBytes($entry.Value, $changedBytes)
                Assert-SelfTestThrows {
                    $changedClosure = Get-GateSourceClosureIdentity `
                        $sourceClosureMain $sourceClosureSelfTest $sourceClosureSummary
                    Assert-GateSourceClosureUnchanged `
                        $sourceClosureFirst $changedClosure "during a source mutation"
                } $entry.Key
            } finally {
                [IO.File]::WriteAllBytes($entry.Value, $originalBytes)
            }
        }
        $missingBytes = [IO.File]::ReadAllBytes($sourceClosureSummary)
        try {
            Remove-Item -LiteralPath $sourceClosureSummary -Force
            Assert-SelfTestThrows {
                Get-GateSourceClosureIdentity `
                    $sourceClosureMain $sourceClosureSelfTest $sourceClosureSummary
            } "scripts/run-realtime-gate-summary.ps1"
        } finally {
            [IO.File]::WriteAllBytes($sourceClosureSummary, $missingBytes)
        }
        Assert-SelfTestThrows {
            Get-GateSourceMemberIdentity "scripts/unexpected.ps1" $sourceClosureMain
        } "Invalid gate source label"
    } finally {
        Remove-Item -LiteralPath $sourceClosureRoot -Recurse -Force
    }

    $movedSummaryProbe = Get-RoleSummary "self-test" "486" @(
        [pscustomobject]@{
            wall_seconds = 2.0
            guest_seconds = 3.0
            real_time_factor = 1.5
            instructions_per_host_second = 4.0
            direct_native_coverage = 0.75
            direct_slow_exits_per_100_instructions = 0.25
        }
    ) $false
    if ($movedSummaryProbe.median.wall_seconds -ne 2.0 -or
        $movedSummaryProbe.median.instructions_per_host_second -ne 4.0) {
        throw "The sourced role summary function returned the wrong medians."
    }
    Write-Host "run-realtime-gate self-test passed"
}
