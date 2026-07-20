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
    Assert-DirectQuakeCampaignMode `
        $false $false $false $false $false $false $false $false $false $false $false `
        "Noise" 6 "Quake" 8 "C:\gate.lock"
    Assert-DirectQuakeCampaignMode `
        $false $false $false $false $false $false $false $false $false $false $false `
        "Screen" 2 "Quake" 8 "C:\gate.lock"
    Assert-DirectQuakeCampaignMode `
        $false $false $false $false $false $false $false $false $false $false $false `
        "Proof" 12 "Quake" 8 "C:\gate.lock"
    if ((Get-NormalizedDirectQuakeCampaignStage "noise") -cne "Noise" -or
        (Get-NormalizedDirectQuakeCampaignStage "SCREEN") -cne "Screen" -or
        (Get-NormalizedDirectQuakeCampaignStage "Proof") -cne "Proof") {
        throw "Direct Quake campaign stage normalization is not canonical."
    }
    Assert-DirectQuakeExecutableRelation "Noise" ("1" * 64) ("2" * 64)
    foreach ($stage in @("Screen", "Proof")) {
        Assert-SelfTestThrows {
            Assert-DirectQuakeExecutableRelation $stage ("1" * 64) ("1" * 64)
        } "require different candidate and retained-parent binaries"
    }
    Assert-SelfTestThrows {
        Assert-DirectQuakeCampaignMode `
            $false $true $false $false $false $false $false $false $false $false $false `
            "Proof" 6 "Quake" 8 "C:\gate.lock"
    } "another comparison mode"
    Assert-SelfTestThrows {
        Assert-DirectQuakeCampaignMode `
            $false $false $false $false $false $false $false $false $false $false $true `
            "Proof" 6 "Quake" 8 "C:\gate.lock"
    } "fixes its order"
    Assert-SelfTestThrows {
        Assert-DirectQuakeCampaignMode `
            $false $false $false $false $false $false $false $false $false $false $false `
            "Screen" 6 "Quake" 8 "C:\gate.lock"
    } "invalid measured-pair count"
    Assert-SelfTestThrows {
        Assert-DirectQuakeCampaignMode `
            $false $false $false $false $false $false $false $false $false $false $false `
            "Proof" 6 "Both" 8 "C:\gate.lock"
    } "requires the Quake workload"
    $campaignPairs = @(1..6 | ForEach-Object {
        (Get-DirectQuakePairOrder $_ @("A", "B")) -join ""
    })
    if (($campaignPairs -join ",") -cne "AB,BA,BA,AB,AB,BA") {
        throw "The Direct Quake campaign pair order is not ABBA, BAAB, ABBA."
    }
    $campaignPairs12 = @(1..12 | ForEach-Object {
        (Get-DirectQuakePairOrder $_ @("A", "B")) -join ""
    })
    if (($campaignPairs12[0..5] -join ",") -cne
        ($campaignPairs12[6..11] -join ",")) {
        throw "The twelve-pair Direct Quake schedule does not repeat the fixed six-pair order."
    }
    $directPolicy = Get-DirectQuakeExecutionPolicy
    if ($directPolicy.environment.IZARRAVM_JIT -cne "1" -or
        $directPolicy.environment.IZARRAVM_POLL_SKIP -cne "0" -or
        $directPolicy.required_zero_counters -cnotcontains "jit_clif_entries" -or
        $directPolicy.required_zero_counters -cnotcontains "jit_region_entries") {
        throw "The Direct Quake execution policy does not exclude legacy backend activity."
    }
    if ((Get-DirectQuakeCampaignMetric ([double[]](1.03) * 6) "Proof").classification -cne
        "normal_promotion_threshold_met" -or
        (Get-DirectQuakeCampaignMetric ([double[]](1.015) * 6) "Proof").classification -cne
            "narrow_requires_mechanism_evidence" -or
        (Get-DirectQuakeCampaignMetric `
            ([double[]](1.01, 1.01, 1.01, 1.01, 1.01, 0.99)) "Proof").classification -cne
            "twelve_pair_extension_eligible" -or
        (Get-DirectQuakeCampaignMetric ([double[]](0.99) * 6) "Proof").classification -cne
            "reject" -or
        (Get-DirectQuakeCampaignMetric `
            ([double[]](1.019, 1.019, 1.019, 1.019, 0.999, 0.999)) "Proof").classification -cne
            "reject" -or
        (Get-DirectQuakeCampaignMetric ([double[]](1.0) * 6) "Noise").classification -cne
            "noise_only") {
        throw "The Direct Quake campaign metric classifier is wrong."
    }
    $hddTreeSelfTestRoot = Join-Path ([IO.Path]::GetTempPath()) (
        "izarra-hdd-tree-$([guid]::NewGuid().ToString('N'))"
    )
    try {
        New-Item -ItemType Directory -Path (Join-Path $hddTreeSelfTestRoot "B") -Force |
            Out-Null
        [IO.File]::WriteAllBytes((Join-Path $hddTreeSelfTestRoot "z.bin"), [byte[]](1, 2, 3))
        [IO.File]::WriteAllBytes((Join-Path $hddTreeSelfTestRoot "B/a.bin"), [byte[]](4, 5))
        $hddTreeFirst = Get-HddTreeSnapshotV1 $hddTreeSelfTestRoot
        (Get-Item -LiteralPath (Join-Path $hddTreeSelfTestRoot "z.bin")).LastWriteTimeUtc =
            [DateTime]::UtcNow.AddDays(-2)
        $hddTreeMetadataOnly = Get-HddTreeSnapshotV1 $hddTreeSelfTestRoot
        if ($hddTreeFirst.tree_sha256 -cne $hddTreeMetadataOnly.tree_sha256 -or
            ($hddTreeFirst.files.path -join ",") -cne "B/a.bin,z.bin") {
            throw "The HDD tree snapshot depends on metadata or non-ordinal enumeration order."
        }
        [IO.File]::WriteAllBytes((Join-Path $hddTreeSelfTestRoot "z.bin"), [byte[]](1, 2, 4))
        if ((Get-HddTreeSnapshotV1 $hddTreeSelfTestRoot).tree_sha256 -ceq
            $hddTreeFirst.tree_sha256) {
            throw "The HDD tree snapshot ignored a content change."
        }
    } finally {
        Remove-Item -LiteralPath $hddTreeSelfTestRoot -Recurse -Force -ErrorAction SilentlyContinue
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
    Assert-TrackMComparisonMode `
        $false $false $false $false $false $false "automatic" `
        $true $false 6 "Both" 8 "C:\gate.lock"
    Assert-TrackMComparisonMode `
        $false $false $false $false $false $false "interpreter" `
        $false $true 6 "Both" 8 "C:\gate.lock"
    $trackMModeFailures = [ordered]@{
        backend = { Assert-TrackMComparisonMode $true $false $false $false $false $false "automatic" $true $false 6 "Both" 8 "C:\gate.lock" }
        report = { Assert-TrackMComparisonMode $false $true $false $false $false $false "automatic" $true $false 6 "Both" 8 "C:\gate.lock" }
        baseline = { Assert-TrackMComparisonMode $false $false $true $false $false $false "automatic" $true $false 6 "Both" 8 "C:\gate.lock" }
        jit = { Assert-TrackMComparisonMode $false $false $false $true $false $false "automatic" $true $false 6 "Both" 8 "C:\gate.lock" }
        executable = { Assert-TrackMComparisonMode $false $false $false $false $true $false "automatic" $true $false 6 "Both" 8 "C:\gate.lock" }
        skip = { Assert-TrackMComparisonMode $false $false $false $false $false $true "automatic" $true $false 6 "Both" 8 "C:\gate.lock" }
        role = { Assert-TrackMComparisonMode $false $false $false $false $false $false "" $true $false 6 "Both" 8 "C:\gate.lock" }
        workload = { Assert-TrackMComparisonMode $false $false $false $false $false $false "automatic" $true $false 6 "Quake" 8 "C:\gate.lock" }
        processor = { Assert-TrackMComparisonMode $false $false $false $false $false $false "automatic" $true $false 6 "Both" 7 "C:\gate.lock" }
        lock = { Assert-TrackMComparisonMode $false $false $false $false $false $false "automatic" $true $false 6 "Both" 8 "gate.lock" }
        screening_runs = { Assert-TrackMComparisonMode $false $false $false $false $false $false "automatic" $true $true 6 "Both" 8 "C:\gate.lock" }
        confirmation_runs = { Assert-TrackMComparisonMode $false $false $false $false $false $false "automatic" $false $true 3 "Both" 8 "C:\gate.lock" }
    }
    foreach ($case in $trackMModeFailures.GetEnumerator()) {
        Assert-SelfTestThrows $case.Value "Track M"
    }
    Assert-PollSkipComparisonMode `
        $false $false $false $false $false $false $false $false $false `
        6 "Doom586" 8 "C:\gate.lock"
    Assert-PollSkipComparisonMode `
        $false $false $false $false $false $false $false $false $false `
        12 "Doom586" 8 "C:\gate.lock"
    $pollSkipModeFailures = [ordered]@{
        backend = { Assert-PollSkipComparisonMode $true $false $false $false $false $false $false $false $false 6 "Doom586" 8 "C:\gate.lock" }
        track_m = { Assert-PollSkipComparisonMode $false $true $false $false $false $false $false $false $false 6 "Doom586" 8 "C:\gate.lock" }
        report = { Assert-PollSkipComparisonMode $false $false $true $false $false $false $false $false $false 6 "Doom586" 8 "C:\gate.lock" }
        baseline = { Assert-PollSkipComparisonMode $false $false $false $true $false $false $false $false $false 6 "Doom586" 8 "C:\gate.lock" }
        jit = { Assert-PollSkipComparisonMode $false $false $false $false $true $false $false $false $false 6 "Doom586" 8 "C:\gate.lock" }
        executable = { Assert-PollSkipComparisonMode $false $false $false $false $false $true $false $false $false 6 "Doom586" 8 "C:\gate.lock" }
        skip_build = { Assert-PollSkipComparisonMode $false $false $false $false $false $false $true $false $false 6 "Doom586" 8 "C:\gate.lock" }
        execution_role = { Assert-PollSkipComparisonMode $false $false $false $false $false $false $false $true $false 6 "Doom586" 8 "C:\gate.lock" }
        screening = { Assert-PollSkipComparisonMode $false $false $false $false $false $false $false $false $true 6 "Doom586" 8 "C:\gate.lock" }
        six_minus_one = { Assert-PollSkipComparisonMode $false $false $false $false $false $false $false $false $false 5 "Doom586" 8 "C:\gate.lock" }
        twelve_plus_one = { Assert-PollSkipComparisonMode $false $false $false $false $false $false $false $false $false 13 "Doom586" 8 "C:\gate.lock" }
        workload = { Assert-PollSkipComparisonMode $false $false $false $false $false $false $false $false $false 6 "Both" 8 "C:\gate.lock" }
        processor = { Assert-PollSkipComparisonMode $false $false $false $false $false $false $false $false $false 6 "Doom586" 7 "C:\gate.lock" }
        lock = { Assert-PollSkipComparisonMode $false $false $false $false $false $false $false $false $false 6 "Doom586" 8 "gate.lock" }
    }
    foreach ($case in $pollSkipModeFailures.GetEnumerator()) {
        Assert-SelfTestThrows $case.Value "POLL-SKIP comparison"
    }
    $skipOffPolicy = Get-PollSkipExecutionPolicy "skip_off"
    $skipOnPolicy = Get-PollSkipExecutionPolicy "skip_on"
    if ($skipOffPolicy.cli -cne "--interpreter" -or $skipOnPolicy.cli -cne "--interpreter" -or
        $skipOffPolicy.environment.IZARRAVM_JIT -cne "0" -or
        $skipOnPolicy.environment.IZARRAVM_JIT -cne "0" -or
        $skipOffPolicy.environment.IZARRAVM_POLL_SKIP -cne "0" -or
        $skipOnPolicy.environment.IZARRAVM_POLL_SKIP -cne "1") {
        throw "POLL-SKIP role policies do not force the interpreter and exact toggle values."
    }
    Assert-SelfTestThrows {
        Get-PollSkipExecutionPolicy "wrong"
    } "Unknown POLL-SKIP"
    if ((Get-PollSkipWarmupOrder) -join "," -cne "skip_off,skip_on") {
        throw "POLL-SKIP warmups are not fixed to skip_off then skip_on."
    }
    $pollSkipRoles = @("skip_on", "skip_off")
    foreach ($seed in @(2, 3)) {
        $first = @(Get-PairOrder 1 $seed $pollSkipRoles)
        $second = @(Get-PairOrder 2 $seed $pollSkipRoles)
        if ($first[0] -ceq $second[0] -or $first[1] -ceq $second[1]) {
            throw "POLL-SKIP measured role order did not alternate for seed $seed."
        }
    }
    $requiredScrubVariables = @(
        "IZARRAVM_POLL_SKIP", "IZARRAVM_POLL_SKIP_DIAG", "IZARRAVM_UNIT_SIM",
        "IZARRAVM_IO_HIST", "IZARRAVM_PROFILE_ITERS"
    )
    $knownDiagnostics = @(Get-KnownDiagnosticVariables)
    foreach ($name in $requiredScrubVariables) {
        if ($knownDiagnostics -cnotcontains $name) {
            throw "The fixed child environment scrub list is missing $name."
        }
    }
    $childEnvironment = New-IzarraChildEnvironment `
        "C:\isolated-home" $knownDiagnostics $skipOnPolicy.environment
    foreach ($name in @(
        "IZARRAVM_POLL_SKIP_DIAG", "IZARRAVM_UNIT_SIM",
        "IZARRAVM_IO_HIST", "IZARRAVM_PROFILE_ITERS"
    )) {
        if (-not $childEnvironment.ContainsKey($name) -or $null -ne $childEnvironment[$name]) {
            throw "The child environment did not explicitly unset $name."
        }
    }
    if ($childEnvironment.IZARRAVM_JIT -cne "0" -or
        $childEnvironment.IZARRAVM_POLL_SKIP -cne "1" -or
        $childEnvironment.HOME -cne "C:\isolated-home") {
        throw "The child environment did not apply the role after diagnostic scrubbing."
    }
    $automaticPolicy = Get-TrackMExecutionPolicy "automatic"
    $interpreterPolicy = Get-TrackMExecutionPolicy "interpreter"
    if ($automaticPolicy.cli -cne "default automatic backend" -or
        @($automaticPolicy.environment.Keys) -join "," -cne
            "IZARRAVM_JIT,IZARRAVM_POLL_SKIP" -or
        $automaticPolicy.environment.IZARRAVM_JIT -cne "1" -or
        $automaticPolicy.environment.IZARRAVM_POLL_SKIP -cne "0" -or
        $interpreterPolicy.cli -cne "--interpreter" -or
        @($interpreterPolicy.environment.Keys) -join "," -cne
            "IZARRAVM_JIT,IZARRAVM_POLL_SKIP" -or
        $interpreterPolicy.environment.IZARRAVM_JIT -cne "0" -or
        $interpreterPolicy.environment.IZARRAVM_POLL_SKIP -cne "0" -or
        $null -ne $automaticPolicy.PSObject.Properties["jit"] -or
        $null -ne $interpreterPolicy.PSObject.Properties["jit"] -or
        @($automaticPolicy.required_zero_counters) -join "," -cne
            "poll_skip_spans,poll_skip_iterations" -or
        @($interpreterPolicy.required_zero_counters) -join "," -cne
            "poll_skip_spans,poll_skip_iterations") {
        throw "Track M execution policies do not force the requested backend."
    }
    $trackMChildEnvironment = New-IzarraChildEnvironment `
        "C:\track-m-home" $knownDiagnostics $interpreterPolicy.environment
    if ($trackMChildEnvironment.IZARRAVM_JIT -cne "0" -or
        $trackMChildEnvironment.IZARRAVM_POLL_SKIP -cne "0" -or
        $trackMChildEnvironment.IZARRAVM_POLL_SKIP_DIAG -ne $null -or
        $trackMChildEnvironment.HOME -cne "C:\track-m-home") {
        throw "Track M child launch did not derive its isolated environment from the policy."
    }
    $automaticChildEnvironment = New-IzarraChildEnvironment `
        "C:\track-m-auto-home" $knownDiagnostics $automaticPolicy.environment
    if ($automaticChildEnvironment.IZARRAVM_JIT -cne "1" -or
        $automaticChildEnvironment.IZARRAVM_POLL_SKIP -cne "0") {
        throw "Automatic Track M child launch did not preserve the policy environment."
    }
    $candidateCommit = "1" * 40
    $parentCommit = "2" * 40
    if ((Get-TrackMParentFromRevisionLine "$candidateCommit $parentCommit" $candidateCommit) -cne
        $parentCommit) {
        throw "Track M did not derive the unique immediate parent."
    }
    Assert-SelfTestThrows {
        Get-TrackMParentFromRevisionLine $candidateCommit $candidateCommit
    } "root commit"
    Assert-SelfTestThrows {
        Get-TrackMParentFromRevisionLine "$candidateCommit $parentCommit $('3' * 40)" $candidateCommit
    } "exactly one immediate parent"
    Assert-SelfTestThrows {
        Get-TrackMParentFromRevisionLine "$candidateCommit $parentCommit" ("4" * 40)
    } "candidate revision"
    Assert-SelfTestThrows {
        Get-TrackMParentFromRevisionLine "$candidateCommit invalid" $candidateCommit
    } "invalid immediate parent"
    if ((Get-TrackMPairedMetric ([double[]](0.99, 0.99, 0.99))).verdict -ne "pass" -or
        (Get-TrackMPairedMetric ([double[]](0.989, 0.989, 0.989))).verdict -ne "regression" -or
        (Get-TrackMPairedMetric ([double[]](0.90, 0.99, 1.08))).verdict -ne "inconclusive" -or
        (Get-TrackMPairedMetricVerdict 0.99 0.97) -ne "pass" -or
        (Get-TrackMPairedMetricVerdict 0.99 0.969999) -ne "inconclusive" -or
        (Get-TrackMPairedMetricVerdict 0.989999 0.99) -ne "regression" -or
        (Get-PairedMetricVerdict 0.98 0.97) -ne "pass") {
        throw "Track M or generic paired threshold boundaries changed."
    }
    $pollSkipVerdictCases = @(
        @([double]1.000001, [double]1.000001, 6, "improved", "improved", $false),
        @([double]1.000001, [double]1.0, 6, "positive_but_inconclusive", "positive_but_inconclusive", $true),
        @([double]1.0, [double]1.0, 6, "neutral", "neutral", $false),
        @([double]0.98, [double]0.97, 6, "neutral", "neutral", $false),
        @([double]0.979999, [double]1.01, 6, "regression", "regression", $false),
        @([double]1.01, [double]0.969999, 6, "regression", "regression", $false),
        @([double]1.01, [double]1.0, 12, "positive_but_inconclusive", "speedup_not_demonstrated", $false),
        @([double]1.0, [double]1.0, 12, "neutral", "speedup_not_demonstrated", $false),
        @([double]1.01, [double]1.01, 12, "improved", "improved", $false)
    )
    foreach ($case in $pollSkipVerdictCases) {
        $result = Get-PollSkipVerdict $case[0] $case[1] $case[2]
        if ($result.classification -cne $case[3] -or $result.verdict -cne $case[4] -or
            $result.twelve_pair_confirmation_required -ne $case[5]) {
            throw "POLL-SKIP verdict boundary $($case -join ',') was classified incorrectly."
        }
    }
    $pollSkipMetric = Get-PollSkipPairedMetric `
        ([double[]](1.01, 1.01, 1.01, 1.01, 1.01, 1.01)) 6
    if ($pollSkipMetric.verdict -cne "improved" -or
        [Math]::Abs($pollSkipMetric.geometric_mean_ratio - 1.01) -gt 1.0e-12 -or
        $pollSkipMetric.lower_bound_estimand -notlike "*geometric mean*") {
        throw "POLL-SKIP paired metrics do not expose the log-ratio estimand."
    }
    Assert-SelfTestThrows {
        Get-PollSkipVerdict ([double]::NaN) 1.0 6
    } "finite and positive"
    Assert-SelfTestThrows {
        Get-PollSkipPairedMetric ([double[]](1, 1, 1, 1, 1, 1)) 12
    } "6 or 12 ratios"
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
        scaled_bus_clocks = 17
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
    $exactSampleB.raw_bus_clocks = 19
    $exactSampleB.scaled_bus_clocks = 16
    $scaledComparison = Compare-EqualWorkRecords `
        (Get-EqualWorkRecord "doom-586" $exactSampleA) `
        (Get-EqualWorkRecord "doom-586" $exactSampleB)
    if ($scaledComparison.matches -or
        $scaledComparison.mismatched_fields.Count -ne 1 -or
        $scaledComparison.mismatched_fields[0] -cne "scaled_bus_clocks") {
        throw "Equal-work comparison did not enforce scaled bus clocks independently."
    }
    $exactSampleA.PSObject.Properties.Remove("scaled_bus_clocks")
    $exactSampleB.PSObject.Properties.Remove("scaled_bus_clocks")
    $missingScaledComparison = Compare-EqualWorkRecords `
        (Get-EqualWorkRecord "doom-586" $exactSampleA) `
        (Get-EqualWorkRecord "doom-586" $exactSampleB)
    if ($missingScaledComparison.matches -or
        $missingScaledComparison.mismatched_fields.Count -ne 1 -or
        $missingScaledComparison.mismatched_fields[0] -cne "scaled_bus_clocks") {
        throw "Equal-work comparison accepted two missing scaled bus totals."
    }
    $exactSampleA | Add-Member -NotePropertyName scaled_bus_clocks -NotePropertyValue $null
    $exactSampleB | Add-Member -NotePropertyName scaled_bus_clocks -NotePropertyValue $null
    $nullScaledComparison = Compare-EqualWorkRecords `
        (Get-EqualWorkRecord "doom-586" $exactSampleA) `
        (Get-EqualWorkRecord "doom-586" $exactSampleB)
    if ($nullScaledComparison.matches -or
        $nullScaledComparison.mismatched_fields.Count -ne 1 -or
        $nullScaledComparison.mismatched_fields[0] -cne "scaled_bus_clocks") {
        throw "Equal-work comparison accepted two null scaled bus totals."
    }
    $exactSampleA.scaled_bus_clocks = 17
    $exactSampleB.scaled_bus_clocks = 17
    $pollExactA = $exactSampleA | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    $pollExactA.perf | Add-Member -NotePropertyName poll_skip_spans -NotePropertyValue 0
    $pollExactA.perf | Add-Member -NotePropertyName poll_skip_iterations -NotePropertyValue 0
    $pollExactA | Add-Member `
        -NotePropertyName gate_measurement_fixture_sha256 `
        -NotePropertyValue ("b" * 64)
    $pollExactB = $pollExactA | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    $pollExactB.perf.instructions = 5
    $pollExactB.perf.poll_skip_spans = 7
    $pollExactB.perf.poll_skip_iterations = 11
    $pollExactComparison = Compare-EqualWorkRecords `
        (Get-PollSkipExactWorkRecord "doom-586" $pollExactA) `
        (Get-PollSkipExactWorkRecord "doom-586" $pollExactB)
    if (-not $pollExactComparison.matches) {
        throw "POLL-SKIP exact work compared retired instructions or poll counters."
    }
    $pollExactRecord = Get-PollSkipExactWorkRecord "doom-586" $pollExactA
    foreach ($field in $pollExactRecord.Keys) {
        $mutatedRecord = [ordered]@{}
        foreach ($entry in $pollExactRecord.GetEnumerator()) {
            $mutatedRecord[$entry.Key] = $entry.Value
        }
        $mutatedRecord[$field] = "changed-$field"
        $fieldComparison = Compare-EqualWorkRecords $pollExactRecord $mutatedRecord
        if ($fieldComparison.matches -or
            $fieldComparison.mismatched_fields.Count -ne 1 -or
            $fieldComparison.mismatched_fields[0] -cne $field) {
            throw "POLL-SKIP exact-work field $field was not enforced independently."
        }
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

    $newTrackMSample = {
        param($Policy, [string]$RevisionRole, [string]$Observation, $ExecutionPolicy, [double]$Ratio)
        $isQuake = $Policy.name -ceq "quake-586"
        $automatic = $ExecutionPolicy.name -ceq "automatic"
        $direct = $ExecutionPolicy.name -ceq "direct"
        $nativeDirect = $automatic -or $direct
        $fixture = if ($isQuake) {
            $syntheticQuakeFixture | ConvertTo-Json -Depth 8 | ConvertFrom-Json
        } else {
            $null
        }
        $completion = if ($isQuake) {
            $syntheticQuakeCompletion | ConvertTo-Json -Depth 8 | ConvertFrom-Json
        } else {
            $null
        }
        $fixtureHash = if ($isQuake) {
            $fixture.prelaunch_overridden_tree_sha256
        } else {
            "c" * 64
        }
        return [pscustomobject][ordered]@{
            wall_seconds = 10.0
            guest_seconds = 10.0 * $Ratio
            real_time_factor = $Ratio
            instructions_per_host_second = 100.0 * $Ratio
            direct_native_coverage = if ($nativeDirect) { 0.9 } else { 0.0 }
            direct_slow_exits_per_100_instructions = 0.0
            perf = [pscustomobject][ordered]@{
                instructions = [uint64]1000
                jit_region_entries = if ($automatic) { 1 } else { 0 }
                jit_region_insns = if ($automatic) { 100 } else { 0 }
                jit_native_insns = if ($automatic) { 100 } else { 0 }
                jit_helper_exits = 0
                jit_native_memory_helpers = 0
                jit_direct_entries = if ($nativeDirect) { 90 } else { 0 }
                jit_direct_insns = if ($nativeDirect) { 900 } else { 0 }
                jit_direct_side_exits = 0
                jit_clif_compile_attempts = 0
                jit_clif_units_installed = 0
                jit_clif_entries = 0
                jit_clif_side_exits = 0
                poll_skip_spans = 0
                poll_skip_iterations = 0
            }
            master_ticks = [uint64]2000
            elapsed_budget_clocks = [uint64]3000
            executed_cpu_core_clocks = [uint64]1100
            raw_bus_clocks = [uint64]1900
            scaled_bus_clocks = [uint64]1700
            stop = [pscustomobject]@{ kind = "test_exit"; code = 0 }
            timedemo = if ($isQuake) { $null } else {
                [pscustomobject]@{ gametics = 2134; realtics = 830 }
            }
            quake_timedemo_identity_count = if ($isQuake) { 1 } else { $null }
            quake_timedemo = if ($isQuake) { $completion.timedemo } else { $null }
            gate_quake_completion = $completion
            gate_fixture = $fixture
            gate_process_exit_code = 0
            gate_role = $RevisionRole
            gate_observation = $Observation
            gate_processor_index = 8
            gate_processor_affinity_mask = "0x0000000000000100"
            gate_processor_affinity_verified = $true
            gate_execution_role = $ExecutionPolicy.name
            gate_execution_cli = $ExecutionPolicy.cli
            gate_execution_jit = $ExecutionPolicy.environment.IZARRAVM_JIT
            gate_poll_skip = $ExecutionPolicy.environment.IZARRAVM_POLL_SKIP
            gate_measurement_fixture_sha256 = $fixtureHash
            gate_termination_policy = "lotura_test_exit"
            gate_artifacts = [pscustomobject][ordered]@{
                profile_json_file = "$($Policy.name)-$RevisionRole-$Observation.json"
                profile_json_sha256 = "4" * 64
                stdout_file = "$($Policy.name)-$RevisionRole-$Observation.stdout.log"
                stdout_sha256 = "5" * 64
                stderr_file = "$($Policy.name)-$RevisionRole-$Observation.stderr.log"
                stderr_sha256 = "6" * 64
                qconsole_file = if ($isQuake) {
                    "$($Policy.name)-$RevisionRole-$Observation-qconsole.log"
                } else {
                    $null
                }
                qconsole_sha256 = if ($isQuake) { "8" * 64 } else { $null }
                result_block_status = "valid"
                result_block_count = 1
                result_block_sha256 = "7" * 64
                result_block_normalized_bytes = 128
            }
        }
    }
    $newDirectQuakeSample = {
        param(
            [string]$RevisionRole,
            [string]$Observation,
            [string]$ObservationClass,
            [double]$Ratio,
            [string]$ExecutableSha256
        )
        $policy = Get-WorkloadPolicy "quake-586"
        $sample = & $newTrackMSample `
            $policy $RevisionRole $Observation $directPolicy $Ratio
        $sample | Add-Member -NotePropertyName gate_observation_class `
            -NotePropertyValue $ObservationClass
        $powerScheme = "Power Scheme GUID: 8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c (High performance)"
        $sample | Add-Member -NotePropertyName gate_power_scheme_before `
            -NotePropertyValue $powerScheme
        $sample | Add-Member -NotePropertyName gate_power_scheme_after `
            -NotePropertyValue $powerScheme
        $sample | Add-Member -NotePropertyName gate_argv `
            -NotePropertyValue ([object[]]@("--cpu", "586", "--cycles", "6200000000"))
        $sample | Add-Member -NotePropertyName gate_argv_sha256 `
            -NotePropertyValue ("a" * 64)
        $sample | Add-Member -NotePropertyName gate_executable_sha256 `
            -NotePropertyValue $ExecutableSha256
        $sample | Add-Member -NotePropertyName gate_hdd_tree `
            -NotePropertyValue ([pscustomobject][ordered]@{
                schema = "izarra-hdd-tree-snapshot-v1"
                tree_sha256 = "b" * 64
            })
        $sample.gate_artifacts | Add-Member -NotePropertyName hdd_tree_file `
            -NotePropertyValue "$RevisionRole-$Observation-hdd-tree.json"
        $sample.gate_artifacts | Add-Member -NotePropertyName hdd_tree_sha256 `
            -NotePropertyValue ("c" * 64)
        if ($ObservationClass -ceq "production") {
            $sample.stop = [pscustomobject]@{
                kind = "cycle_limit"
                requested = [uint64]6200000000
            }
            $sample.gate_termination_policy = "fixed_cycle_production"
            $sample.gate_fixture = $null
            $sample.gate_quake_completion = $null
            $sample.gate_measurement_fixture_sha256 = "d" * 64
        }
        return $sample
    }
    $savedRunsForDirectSelfTest = $Runs
    try {
        $Runs = 6
        $candidateSha = "1" * 64
        $parentSha = "2" * 64
        $directCandidate = @()
        $directParent = @()
        foreach ($pair in 1..6) {
            $directCandidate += & $newDirectQuakeSample `
                "candidate" "pair$pair" "production" 1.03 $candidateSha
            $directParent += & $newDirectQuakeSample `
                "parent" "pair$pair" "production" 1.0 $parentSha
        }
        $directWarmups = [pscustomobject][ordered]@{
            candidate = [object[]]@(& $newDirectQuakeSample `
                "candidate" "warmup" "production" 1.0 $candidateSha)
            parent = [object[]]@(& $newDirectQuakeSample `
                "parent" "warmup" "production" 1.0 $parentSha)
        }
        $directCorrectness = [pscustomobject][ordered]@{
            candidate = [object[]]@(& $newDirectQuakeSample `
                "candidate" "correctness" "correctness" 1.0 $candidateSha)
            parent = [object[]]@(& $newDirectQuakeSample `
                "parent" "correctness" "correctness" 1.0 $parentSha)
        }
        $directWorkload = Get-DirectQuakeCampaignWorkloadSummary `
            (Get-WorkloadPolicy "quake-586") $directCandidate $directParent `
            $directWarmups $directCorrectness $directPolicy "Proof" $candidateSha $parentSha
        if ($directWorkload.exact_work.verdict -cne "pass" -or
            $directWorkload.provenance.verdict -cne "pass" -or
            $directWorkload.paired_metrics.real_time_factor.classification -cne
                "normal_promotion_threshold_met") {
            throw "The Direct Quake campaign synthetic proof was rejected."
        }
        $noiseCandidate = @()
        $noiseParent = @()
        foreach ($pair in 1..6) {
            $noiseCandidate += & $newDirectQuakeSample `
                "candidate" "pair$pair" "production" 1.0 $parentSha
            $noiseParent += & $newDirectQuakeSample `
                "parent" "pair$pair" "production" 1.0 $parentSha
        }
        $noiseWarmups = [pscustomobject][ordered]@{
            candidate = [object[]]@(& $newDirectQuakeSample `
                "candidate" "warmup" "production" 1.0 $parentSha)
            parent = [object[]]@(& $newDirectQuakeSample `
                "parent" "warmup" "production" 1.0 $parentSha)
        }
        $noiseCorrectness = [pscustomobject][ordered]@{
            candidate = [object[]]@(& $newDirectQuakeSample `
                "candidate" "correctness" "correctness" 1.0 $parentSha)
            parent = [object[]]@(& $newDirectQuakeSample `
                "parent" "correctness" "correctness" 1.0 $parentSha)
        }
        $noiseWorkload = Get-DirectQuakeCampaignWorkloadSummary `
            (Get-WorkloadPolicy "quake-586") $noiseCandidate $noiseParent `
            $noiseWarmups $noiseCorrectness $directPolicy "Noise" $candidateSha $parentSha
        if ($noiseWorkload.exact_work.verdict -cne "pass" -or
            $noiseWorkload.provenance.verdict -cne "pass" -or
            $noiseWorkload.paired_metrics.real_time_factor.classification -cne "noise_only") {
            throw "The Direct Quake single-executable noise study was rejected."
        }
        $noiseCandidate[0].gate_executable_sha256 = $candidateSha
        $escapedNoiseWorkload = Get-DirectQuakeCampaignWorkloadSummary `
            (Get-WorkloadPolicy "quake-586") $noiseCandidate $noiseParent `
            $noiseWarmups $noiseCorrectness $directPolicy "Noise" $candidateSha $parentSha
        if ($escapedNoiseWorkload.provenance.verdict -cne "fail" -or
            ($escapedNoiseWorkload.provenance.failure_reasons -join " ") -notmatch
                "wrong frozen binary") {
            throw "The Direct Quake noise study accepted a candidate-binary observation."
        }
        $noiseCandidate[0].gate_executable_sha256 = $parentSha
        $directCandidate[0].gate_hdd_tree.tree_sha256 = "e" * 64
        $hddMismatch = Get-DirectQuakeCampaignWorkloadSummary `
            (Get-WorkloadPolicy "quake-586") $directCandidate $directParent `
            $directWarmups $directCorrectness $directPolicy "Proof" $candidateSha $parentSha
        if ($hddMismatch.exact_work.verdict -cne "fail" -or
            ($hddMismatch.exact_work.failure_reasons -join " ") -notmatch "hdd_tree_identity") {
            throw "The Direct Quake campaign accepted a final HDD mismatch."
        }
        $directCandidate[0].gate_hdd_tree.tree_sha256 = "b" * 64
        $directCandidate[0].perf.jit_clif_entries = 1
        $clifMismatch = Get-DirectQuakeCampaignWorkloadSummary `
            (Get-WorkloadPolicy "quake-586") $directCandidate $directParent `
            $directWarmups $directCorrectness $directPolicy "Proof" $candidateSha $parentSha
        if ($clifMismatch.provenance.verdict -cne "fail" -or
            ($clifMismatch.provenance.failure_reasons -join " ") -notmatch "Clif activity") {
            throw "The Direct Quake campaign accepted legacy Clif activity."
        }
        $directCandidate[0].perf.jit_clif_entries = 0
        $directCandidate[0].PSObject.Properties.Remove("scaled_bus_clocks")
        $missingScaledBus = Get-DirectQuakeCampaignWorkloadSummary `
            (Get-WorkloadPolicy "quake-586") $directCandidate $directParent `
            $directWarmups $directCorrectness $directPolicy "Proof" $candidateSha $parentSha
        if ($missingScaledBus.provenance.verdict -cne "fail" -or
            ($missingScaledBus.provenance.failure_reasons -join " ") -notmatch
                "scaled_bus_clocks") {
            throw "The Direct Quake campaign accepted a missing scaled bus total."
        }
        $directCandidate[0] | Add-Member `
            -NotePropertyName scaled_bus_clocks -NotePropertyValue ([uint64]1700)
    } finally {
        $Runs = $savedRunsForDirectSelfTest
    }
    $newTrackMScreen = {
        param($Policy, $ExecutionPolicy, [int]$Pairs, [double[]]$Ratios)
        $candidate = @()
        $parent = @()
        foreach ($pair in 1..$Pairs) {
            $candidate += & $newTrackMSample `
                $Policy "candidate" "pair$pair" $ExecutionPolicy $Ratios[$pair - 1]
            $parent += & $newTrackMSample $Policy "parent" "pair$pair" $ExecutionPolicy 1.0
        }
        return [pscustomobject][ordered]@{
            candidate = [object[]]$candidate
            parent = [object[]]$parent
            warmups = [pscustomobject][ordered]@{
                candidate = [object[]]@(& $newTrackMSample `
                    $Policy "candidate" "warmup" $ExecutionPolicy 1.0)
                parent = [object[]]@(& $newTrackMSample `
                    $Policy "parent" "warmup" $ExecutionPolicy 1.0)
            }
        }
    }
    $trackMPolicies = @(Get-WorkloadPolicies "Both")
    $trackMAutomaticWorkloads = @()
    foreach ($policy in $trackMPolicies) {
        $screen = & $newTrackMScreen $policy $automaticPolicy 3 ([double[]](1, 1, 1))
        $result = Get-TrackMWorkloadSummary `
            $policy $screen.candidate $screen.parent $screen.warmups $automaticPolicy
        if (@($result.verdicts.Values | Where-Object { $_ -ne "pass" }).Count -ne 0) {
            throw "$($policy.name) automatic Track M pass screen was rejected."
        }
        $trackMAutomaticWorkloads += $result
    }
    $interpreterScreen = & $newTrackMScreen `
        $trackMPolicies[2] $interpreterPolicy 6 ([double[]](1, 1, 1, 1, 1, 1))
    $interpreterResult = Get-TrackMWorkloadSummary `
        $trackMPolicies[2] $interpreterScreen.candidate $interpreterScreen.parent `
        $interpreterScreen.warmups $interpreterPolicy
    if (@($interpreterResult.verdicts.Values | Where-Object { $_ -ne "pass" }).Count -ne 0) {
        throw "The six-pair interpreter Track M pass screen was rejected."
    }

    $exactMutations = [ordered]@{
        instructions = { param($sample) $sample.perf.instructions++ }
        master_ticks = { param($sample) $sample.master_ticks++ }
        elapsed_budget_clocks = { param($sample) $sample.elapsed_budget_clocks++ }
        executed_cpu_core_clocks = { param($sample) $sample.executed_cpu_core_clocks++ }
        raw_bus_clocks = { param($sample) $sample.raw_bus_clocks++ }
        scaled_bus_clocks = { param($sample) $sample.scaled_bus_clocks++ }
        stop = { param($sample) $sample.stop.code = 1 }
        timedemo_identity = { param($sample) $sample.quake_timedemo.line = "969 frames  22.7 seconds  42.6 fps" }
        result_block_identity = { param($sample) $sample.gate_artifacts.result_block_sha256 = "9" * 64 }
        measurement_fixture_identity = { param($sample) $sample.gate_fixture.prelaunch_overridden_tree_sha256 = "9" * 64 }
        quake_completion_identity = { param($sample) $sample.gate_quake_completion.wait_marker_count = 0 }
        qconsole_sha256 = { param($sample) $sample.gate_artifacts.qconsole_sha256 = "9" * 64 }
    }
    foreach ($mutation in $exactMutations.GetEnumerator()) {
        $screen = & $newTrackMScreen `
            $trackMPolicies[2] $automaticPolicy 3 ([double[]](1, 1, 1))
        & $mutation.Value $screen.candidate[0]
        $result = Get-TrackMWorkloadSummary `
            $trackMPolicies[2] $screen.candidate $screen.parent $screen.warmups $automaticPolicy
        if ($result.verdicts.exact_work -ne "fail") {
            throw "Track M exact-work mutation $($mutation.Key) was accepted."
        }
    }
    $warmupMismatchScreen = & $newTrackMScreen `
        $trackMPolicies[0] $automaticPolicy 3 ([double[]](1, 1, 1))
    $warmupMismatchScreen.warmups.candidate[0].raw_bus_clocks++
    $warmupMismatchResult = Get-TrackMWorkloadSummary `
        $trackMPolicies[0] $warmupMismatchScreen.candidate $warmupMismatchScreen.parent `
        $warmupMismatchScreen.warmups $automaticPolicy
    if ($warmupMismatchResult.verdicts.exact_work -ne "fail") {
        throw "Track M accepted unequal discarded warmups."
    }
    $missingScaledScreen = & $newTrackMScreen `
        $trackMPolicies[0] $automaticPolicy 3 ([double[]](1, 1, 1))
    foreach ($sample in @(
        $missingScaledScreen.candidate + $missingScaledScreen.parent +
        $missingScaledScreen.warmups.candidate + $missingScaledScreen.warmups.parent
    )) {
        $sample.PSObject.Properties.Remove("scaled_bus_clocks")
    }
    $missingScaledResult = Get-TrackMWorkloadSummary `
        $trackMPolicies[0] $missingScaledScreen.candidate $missingScaledScreen.parent `
        $missingScaledScreen.warmups $automaticPolicy
    if ($missingScaledResult.verdicts.exact_work -ne "fail") {
        throw "Track M accepted scaled bus totals missing from both roles."
    }
    $nullScaledScreen = & $newTrackMScreen `
        $trackMPolicies[0] $automaticPolicy 3 ([double[]](1, 1, 1))
    foreach ($sample in @(
        $nullScaledScreen.candidate + $nullScaledScreen.parent +
        $nullScaledScreen.warmups.candidate + $nullScaledScreen.warmups.parent
    )) {
        $sample.scaled_bus_clocks = $null
    }
    $nullScaledResult = Get-TrackMWorkloadSummary `
        $trackMPolicies[0] $nullScaledScreen.candidate $nullScaledScreen.parent `
        $nullScaledScreen.warmups $automaticPolicy
    if ($nullScaledResult.verdicts.exact_work -ne "fail") {
        throw "Track M accepted null scaled bus totals from both roles."
    }

    $semanticMutations = [ordered]@{
        test_exit = { param($sample) $sample.stop.code = 7 }
        host_exit = { param($sample) $sample.gate_process_exit_code = 7 }
        result_block = { param($sample) $sample.gate_artifacts.result_block_status = "invalid" }
        completion_order = { param($sample) $sample.gate_quake_completion.result_before_wait_marker = $false }
        fatal_text = {
            param($sample)
            $sample.gate_quake_completion.fatal_match_count = 1
            $sample.gate_quake_completion.fatal_matches = @("synthetic fatal")
        }
    }
    foreach ($mutation in $semanticMutations.GetEnumerator()) {
        $screen = & $newTrackMScreen `
            $trackMPolicies[2] $automaticPolicy 3 ([double[]](1, 1, 1))
        & $mutation.Value $screen.candidate[0]
        $result = Get-TrackMWorkloadSummary `
            $trackMPolicies[2] $screen.candidate $screen.parent $screen.warmups $automaticPolicy
        if ($result.verdicts.semantic -ne "fail") {
            throw "Track M semantic mutation $($mutation.Key) was accepted."
        }
    }
    $provenanceMutations = [ordered]@{
        observation = { param($sample) $sample.gate_observation = "pair9" }
        fixture = { param($sample) $sample.gate_measurement_fixture_sha256 = "9" * 64 }
        affinity = { param($sample) $sample.gate_processor_index = 7 }
        execution = { param($sample) $sample.gate_execution_jit = "0" }
        poll_skip_policy = { param($sample) $sample.gate_poll_skip = "1" }
        automatic_backend = { param($sample) $sample.perf.jit_direct_entries = 0 }
        result_bytes = { param($sample) $sample.gate_artifacts.result_block_normalized_bytes = 0 }
    }
    foreach ($mutation in $provenanceMutations.GetEnumerator()) {
        $screen = & $newTrackMScreen `
            $trackMPolicies[2] $automaticPolicy 3 ([double[]](1, 1, 1))
        & $mutation.Value $screen.candidate[0]
        $result = Get-TrackMWorkloadSummary `
            $trackMPolicies[2] $screen.candidate $screen.parent $screen.warmups $automaticPolicy
        if ($result.verdicts.provenance -ne "fail") {
            throw "Track M provenance mutation $($mutation.Key) was accepted."
        }
    }
    $trackMSampleLocations = [ordered]@{
        candidate_warmup = { param($screen) $screen.warmups.candidate[0] }
        parent_warmup = { param($screen) $screen.warmups.parent[0] }
        candidate_pair = { param($screen) $screen.candidate[0] }
        parent_pair = { param($screen) $screen.parent[0] }
    }
    $zeroCounterMutations = [ordered]@{
        nonzero = { param($sample, $field) $sample.perf.$field = 1 }
        missing = { param($sample, $field) $sample.perf.PSObject.Properties.Remove($field) }
    }
    foreach ($field in @($automaticPolicy.required_zero_counters)) {
        foreach ($location in $trackMSampleLocations.GetEnumerator()) {
            foreach ($mutation in $zeroCounterMutations.GetEnumerator()) {
                $screen = & $newTrackMScreen `
                    $trackMPolicies[0] $automaticPolicy 3 ([double[]](1, 1, 1))
                $sample = & $location.Value $screen
                & $mutation.Value $sample $field
                $result = Get-TrackMWorkloadSummary `
                    $trackMPolicies[0] $screen.candidate $screen.parent `
                    $screen.warmups $automaticPolicy
                if ($result.verdicts.provenance -ne "fail") {
                    throw "Track M accepted $($mutation.Key) $field in $($location.Key)."
                }
            }
        }
    }
    foreach ($field in @(
        "jit_region_entries", "jit_region_insns", "jit_native_insns",
        "jit_direct_entries", "jit_direct_insns", "jit_direct_side_exits"
    )) {
        $interpreterPolarityScreen = & $newTrackMScreen `
            $trackMPolicies[0] $interpreterPolicy 3 ([double[]](1, 1, 1))
        $interpreterPolarityScreen.candidate[0].perf.$field = 1
        $interpreterPolarityResult = Get-TrackMWorkloadSummary `
            $trackMPolicies[0] $interpreterPolarityScreen.candidate `
            $interpreterPolarityScreen.parent $interpreterPolarityScreen.warmups $interpreterPolicy
        if ($interpreterPolarityResult.verdicts.provenance -ne "fail") {
            throw "Track M accepted nonzero interpreter counter $field."
        }
    }

    $revision = "1" * 40
    $baselineCommit = "2" * 40
    $candidateTree = "3" * 40
    $baselineTree = "4" * 40
    $buildFingerprint = "5" * 64
    $candidateArtifact = [pscustomobject][ordered]@{
        executed_copy_path = "C:\evidence\candidate-izarravm.exe"
        sha256 = "6" * 64
        verified = $true
        built_this_invocation = $true
        artifact_source = [pscustomobject]@{
            head_commit = $revision
            head_tree = $candidateTree
        }
        build = [pscustomobject]@{ recipe_fingerprint_sha256 = $buildFingerprint }
    }
    $baselineArtifact = [pscustomobject][ordered]@{
        executed_copy_path = "C:\evidence\parent-izarravm.exe"
        sha256 = "7" * 64
        verified = $true
        built_this_invocation = $true
        artifact_source = [pscustomobject]@{
            head_commit = $baselineCommit
            head_tree = $baselineTree
        }
        build = [pscustomobject]@{ recipe_fingerprint_sha256 = $buildFingerprint }
    }
    $repositoryAtSelection = [pscustomobject]@{
        head_commit = $revision; head_tree = $candidateTree; dirty = $false; status = @()
    }
    $repositoryAtCompletion = $repositoryAtSelection.PSObject.Copy()
    $repositoryStable = $true
    $candidateExecutableStable = $true
    $parentExecutableStable = $true
    $doomFrozenStable = $true
    $quakeFrozenStable = $true
    $doomSourceStable = $true
    $quakeSourceStable = $true
    $gateSourceClosureStable = $true
    $gateScriptHash = "8" * 64
    $gateScriptHashAfter = $gateScriptHash
    $fixtureManifestStable = $true
    $verifiedChildAffinityStable = $true
    $outerAffinityRestoreFailure = $null
    $powerSchemeRecorded = $true
    $powerSchemeStable = $true
    $activePowerSchemeAtCompletion = "test power scheme"
    $MeasurementLockPath = "C:\gate.lock"
    $measurementLockEvidence = [pscustomobject]@{ path = $MeasurementLockPath }
    $detectedBuildEnvironmentOverrides = @{}
    $Runs = 3
    $Screening = $true
    $policies = $trackMPolicies
    $trackMExecutionPolicy = $automaticPolicy
    $verifiedChildAffinityMasks = [Collections.Generic.List[string]]::new()
    foreach ($child in 1..($policies.Count * (2 + 2 * $Runs))) {
        $verifiedChildAffinityMasks.Add("0x0000000000000100")
    }
    $fixtureManifestMatches = [ordered]@{ doom = $true; quake = $true }
    $gateSourceClosureEvidence = [ordered]@{ schema = "test" }
    $fixtureManifestHash = "9" * 64
    $fixtureManifestHashAfter = $fixtureManifestHash
    $workloadInputHashes = [ordered]@{}
    $workloadTreeHashes = [ordered]@{}
    $workloadCanonicalTreeHashes = [ordered]@{}
    $exitVmHash = "a" * 64
    $hostIdentity = [ordered]@{ os = "self-test" }
    $ProcessorIndex = 8
    $requestedProcessorMask = [int64]1 -shl 8
    $PairSeed = 2
    $pairRoles = @("candidate", "parent")
    $trackMSummary = New-TrackMComparisonSummary $trackMAutomaticWorkloads
    if ($trackMSummary.schema -cne "izarravm-track-m-revision-pair-v1" -or
        $trackMSummary.verdict -cne "passed" -or -not $trackMSummary.retention_eligible -or
        $trackMSummary.six_pair_rerun_eligible -or
        $trackMSummary.revision_pair.candidate_commit -cne $revision -or
        $trackMSummary.revision_pair.parent_commit -cne $baselineCommit -or
        $trackMSummary.revision_pair.candidate_tree -cne $candidateTree -or
        $trackMSummary.revision_pair.parent_tree -cne $baselineTree -or
        $trackMSummary.execution.candidate.environment.IZARRAVM_JIT -cne "1" -or
        $trackMSummary.execution.candidate.environment.IZARRAVM_POLL_SKIP -cne "0" -or
        $trackMSummary.execution.parent.environment.IZARRAVM_JIT -cne "1" -or
        $trackMSummary.execution.parent.environment.IZARRAVM_POLL_SKIP -cne "0" -or
        @($trackMSummary.execution.required_zero_counters) -join "," -cne
            "poll_skip_spans,poll_skip_iterations" -or
        $trackMSummary.Contains("accepted_baseline")) {
        throw "The Track M top-level pass summary is incomplete or uses a stale baseline identity."
    }
    $inconclusiveWorkloads = $trackMAutomaticWorkloads |
        ConvertTo-Json -Depth 20 | ConvertFrom-Json
    $inconclusiveWorkloads[0].verdicts.performance = "inconclusive"
    $inconclusiveWorkloads[0].checks.performance.verdict = "inconclusive"
    $inconclusiveWorkloads[0].failure_reasons = @("synthetic lower-bound miss")
    $inconclusiveSummary = New-TrackMComparisonSummary $inconclusiveWorkloads
    if ($inconclusiveSummary.verdict -cne "inconclusive" -or
        $inconclusiveSummary.retention_eligible -or
        -not $inconclusiveSummary.six_pair_rerun_eligible) {
        throw "Track M did not preserve a lower-bound-only screening result as inconclusive."
    }
    $regressionWorkloads = $trackMAutomaticWorkloads |
        ConvertTo-Json -Depth 20 | ConvertFrom-Json
    $regressionWorkloads[0].verdicts.performance = "regression"
    $regressionWorkloads[0].checks.performance.verdict = "regression"
    $regressionWorkloads[0].failure_reasons = @("synthetic median miss")
    $regressionSummary = New-TrackMComparisonSummary $regressionWorkloads
    if ($regressionSummary.verdict -cne "failed" -or
        $regressionSummary.retention_eligible -or $regressionSummary.six_pair_rerun_eligible) {
        throw "Track M did not classify a median miss as a failed retention screen."
    }
    $caseWorkloads = $trackMAutomaticWorkloads
    $globalProvenanceCases = [ordered]@{
        build = [pscustomobject]@{
            mutate = { $candidateArtifact.verified = $false }
            restore = { $candidateArtifact.verified = $true }
        }
        revision = [pscustomobject]@{
            mutate = { $baselineArtifact.artifact_source.head_commit = "f" * 40 }
            restore = { $baselineArtifact.artifact_source.head_commit = $baselineCommit }
        }
        recipe = [pscustomobject]@{
            mutate = { $baselineArtifact.build.recipe_fingerprint_sha256 = "f" * 64 }
            restore = { $baselineArtifact.build.recipe_fingerprint_sha256 = $buildFingerprint }
        }
        repository = [pscustomobject]@{
            mutate = { $repositoryAtSelection.dirty = $true }
            restore = { $repositoryAtSelection.dirty = $false }
        }
        executable = [pscustomobject]@{
            mutate = { $candidateExecutableStable = $false }
            restore = { $candidateExecutableStable = $true }
        }
        workload_tree = [pscustomobject]@{
            mutate = { $doomFrozenStable = $false }
            restore = { $doomFrozenStable = $true }
        }
        source_closure = [pscustomobject]@{
            mutate = { $gateSourceClosureStable = $false }
            restore = { $gateSourceClosureStable = $true }
        }
        workload_manifest = [pscustomobject]@{
            mutate = { $fixtureManifestStable = $false }
            restore = { $fixtureManifestStable = $true }
        }
        child_affinity = [pscustomobject]@{
            mutate = { $verifiedChildAffinityStable = $false }
            restore = { $verifiedChildAffinityStable = $true }
        }
        affinity_restore = [pscustomobject]@{
            mutate = { $outerAffinityRestoreFailure = [Exception]::new("self-test") }
            restore = { $outerAffinityRestoreFailure = $null }
        }
        power = [pscustomobject]@{
            mutate = { $powerSchemeStable = $false }
            restore = { $powerSchemeStable = $true }
        }
        lock = [pscustomobject]@{
            mutate = { $measurementLockEvidence.path = "C:\wrong.lock" }
            restore = { $measurementLockEvidence.path = $MeasurementLockPath }
        }
        build_environment = [pscustomobject]@{
            mutate = { $detectedBuildEnvironmentOverrides["RUSTFLAGS"] = "self-test" }
            restore = { $detectedBuildEnvironmentOverrides.Remove("RUSTFLAGS") }
        }
        workload_count = [pscustomobject]@{
            mutate = { $caseWorkloads = [object[]]$trackMAutomaticWorkloads[0..1] }
            restore = { $caseWorkloads = $trackMAutomaticWorkloads }
        }
    }
    foreach ($case in $globalProvenanceCases.GetEnumerator()) {
        $mutate = $case.Value.mutate
        $restore = $case.Value.restore
        try {
            . $mutate
            $failedSummary = New-TrackMComparisonSummary $caseWorkloads
            if ($failedSummary.verdict -cne "failed" -or
                $failedSummary.retention_eligible -or
                $failedSummary.six_pair_rerun_eligible -or
                $failedSummary.verdicts.provenance -cne "fail") {
                throw "Track M global provenance mutation $($case.Key) was accepted."
            }
        } finally {
            . $restore
        }
    }

    $savedDirectSummaryState = [ordered]@{
        runs = $Runs
        screening = $Screening
        policies = $policies
        masks = $verifiedChildAffinityMasks
        candidate_sha256 = $candidateArtifact.sha256
        parent_sha256 = $baselineArtifact.sha256
        campaign_stage = $CampaignStage
        active_power_scheme_at_completion = $activePowerSchemeAtCompletion
    }
    try {
        $Runs = 6
        $Screening = $false
        $CampaignStage = "Proof"
        $policies = @((Get-WorkloadPolicy "quake-586"))
        $directQuakeExecutionPolicy = $directPolicy
        $candidateArtifact.sha256 = $candidateSha
        $baselineArtifact.sha256 = $parentSha
        $activePowerScheme = `
            "Power Scheme GUID: $highPerformancePowerSchemeGuid (High performance)"
        $activePowerSchemeAtCompletion = $activePowerScheme
        $verifiedChildAffinityMasks = [Collections.Generic.List[string]]::new()
        foreach ($child in 1..16) {
            $verifiedChildAffinityMasks.Add("0x0000000000000100")
        }
        $directCampaignSummary = New-DirectQuakeCampaignSummary @($directWorkload)
        if ($directCampaignSummary.schema -cne
                "izarravm-direct-quake-campaign-partial-proof-v1" -or
            $directCampaignSummary.verdict -cne "normal_promotion_threshold_met" -or
            -not $directCampaignSummary.evidence_valid -or
            $directCampaignSummary.retention_eligible -or
            $directCampaignSummary.retention_blockers.Count -ne 2 -or
            $directCampaignSummary.retention_blockers -notcontains
                "StateSnapshotV1 is not yet captured" -or
            $directCampaignSummary.retention_blockers -notcontains
                "the per-slice deterministic counter allowlist is not yet implemented" -or
            ($directCampaignSummary.retention_blockers -join " ") -match
                "scaled bus-clock") {
            throw "The Direct Quake campaign top-level partial-proof summary is wrong."
        }
        $CampaignStage = "Noise"
        $noiseCampaignSummary = New-DirectQuakeCampaignSummary @($noiseWorkload)
        $noiseExecution = $noiseCampaignSummary.executables.noise_execution
        if ($noiseCampaignSummary.verdict -cne "valid_noise_study" -or
            -not $noiseCampaignSummary.evidence_valid -or
            $noiseCampaignSummary.comparison_class -cne
                "direct_quake_retained_parent_single_executable_aa" -or
            $noiseCampaignSummary.executables.byte_identical -or
            $noiseCampaignSummary.executables.candidate_build_executed -or
            $noiseExecution.sha256 -cne $parentSha -or
            -not $noiseExecution.same_frozen_executable_for_all_roles) {
            throw "The Direct Quake top-level noise evidence is not a single-executable A/A study."
        }
        $candidateArtifact.sha256 = $parentSha
        foreach ($stage in @("Screen", "Proof")) {
            $CampaignStage = $stage
            $equalBuildSummary = New-DirectQuakeCampaignSummary @($directWorkload)
            if ($equalBuildSummary.evidence_valid -or
                ($equalBuildSummary.failure_reasons -join " ") -notmatch
                    "compared byte-identical builds") {
                throw "Direct Quake $stage accepted byte-identical revision builds."
            }
        }
        $candidateArtifact.sha256 = $candidateSha
    } finally {
        $Runs = $savedDirectSummaryState.runs
        $Screening = $savedDirectSummaryState.screening
        $policies = $savedDirectSummaryState.policies
        $verifiedChildAffinityMasks = $savedDirectSummaryState.masks
        $candidateArtifact.sha256 = $savedDirectSummaryState.candidate_sha256
        $baselineArtifact.sha256 = $savedDirectSummaryState.parent_sha256
        $CampaignStage = $savedDirectSummaryState.campaign_stage
        $activePowerSchemeAtCompletion = `
            $savedDirectSummaryState.active_power_scheme_at_completion
    }

    $evidenceRoot = Join-Path ([IO.Path]::GetTempPath()) (
        "izarravm-track-m-evidence-$([guid]::NewGuid().ToString('N'))"
    )
    $evidenceResults = Join-Path $evidenceRoot "results"
    $evidenceSources = Join-Path $evidenceRoot "sources"
    New-Item -ItemType Directory -Path $evidenceResults, $evidenceSources | Out-Null
    try {
        $evidenceCandidatePath = Join-Path $evidenceResults "candidate-izarravm.exe"
        $evidenceParentPath = Join-Path $evidenceResults "parent-izarravm.exe"
        [IO.File]::WriteAllText($evidenceCandidatePath, "candidate executable`n")
        [IO.File]::WriteAllText($evidenceParentPath, "parent executable`n")
        $evidenceCandidate = [pscustomobject]@{
            executed_copy_path = $evidenceCandidatePath
            sha256 = Get-FileSha256 $evidenceCandidatePath
        }
        $evidenceParent = [pscustomobject]@{
            executed_copy_path = $evidenceParentPath
            sha256 = Get-FileSha256 $evidenceParentPath
        }
        $evidenceSummary = [pscustomobject][ordered]@{
            schema = "izarravm-track-m-revision-pair-v1"
            verdict = "passed"
            retention_eligible = $true
            execution = [pscustomobject]@{ role = "automatic" }
            revision_pair = [pscustomobject]@{
                candidate_commit = $revision
                candidate_tree = $candidateTree
                parent_commit = $baselineCommit
                parent_tree = $baselineTree
            }
            workload_manifest_sha256 = [pscustomobject]@{ at_completion = $null }
            workloads = [object[]]@(
                $trackMAutomaticWorkloads[0],
                $trackMAutomaticWorkloads[2]
            )
        }
        foreach ($workload in $evidenceSummary.workloads) {
            foreach ($role in @("candidate", "parent")) {
                $samples = @($workload.discarded_warmups.$role) + @($workload.$role.runs)
                foreach ($sample in $samples) {
                    foreach ($name in @("profile_json", "stdout", "stderr")) {
                        $fileProperty = "${name}_file"
                        $hashProperty = "${name}_sha256"
                        $path = Join-Path $evidenceResults $sample.gate_artifacts.$fileProperty
                        [IO.File]::WriteAllText(
                            $path,
                            "$($workload.name) $role $($sample.gate_observation) $name`n"
                        )
                        $sample.gate_artifacts.$hashProperty = Get-FileSha256 $path
                    }
                    if ($workload.name -ceq "quake-586") {
                        $path = Join-Path $evidenceResults $sample.gate_artifacts.qconsole_file
                        [IO.File]::WriteAllText(
                            $path,
                            "$($workload.name) $role $($sample.gate_observation) qconsole`n"
                        )
                        $sample.gate_artifacts.qconsole_sha256 = Get-FileSha256 $path
                    }
                }
            }
        }
        $evidenceMain = Join-Path $evidenceSources "run-realtime-gate.ps1"
        $evidenceSelfTest = Join-Path $evidenceSources "run-realtime-gate-self-test.ps1"
        $evidenceSummaryScript = Join-Path $evidenceSources "run-realtime-gate-summary.ps1"
        Copy-Item -LiteralPath $gateMainScriptPath -Destination $evidenceMain
        Copy-Item -LiteralPath $gateSelfTestScriptPath -Destination $evidenceSelfTest
        Copy-Item -LiteralPath $gateSummaryScriptPath -Destination $evidenceSummaryScript
        $evidenceClosure = Get-GateSourceClosureIdentity `
            $evidenceMain $evidenceSelfTest $evidenceSummaryScript
        $evidenceFixtureManifest = Join-Path $evidenceSources "realtime-gate-inputs.json"
        [IO.File]::WriteAllText($evidenceFixtureManifest, "{`"schema`":`"self-test`"}`n")
        $evidenceSummary.workload_manifest_sha256.at_completion = `
            Get-FileSha256 $evidenceFixtureManifest
        $evidenceSummaryPath = Join-Path $evidenceResults "summary.json"
        $evidenceSummary | ConvertTo-Json -Depth 20 |
            Set-Content -LiteralPath $evidenceSummaryPath -Encoding utf8
        $evidencePackage = Write-TrackMEvidencePackage `
            $evidenceResults $evidenceSummaryPath $evidenceSummary `
            $evidenceCandidate $evidenceParent $evidenceClosure `
            $evidenceMain $evidenceSelfTest $evidenceSummaryScript $evidenceFixtureManifest
        $manifest = Get-Content -LiteralPath $evidencePackage.manifest_path -Raw | ConvertFrom-Json
        $resultLog = Get-Content -LiteralPath $evidencePackage.result_log_path -Raw
        $manifestPaths = @($manifest.result_directory_files.path)
        if ($manifest.integrity_verdict -cne "pass" -or
            $manifest.gate_source_members.Count -ne 3 -or
            $manifest.result_directory_files.Count -ne 59 -or
            ($manifestPaths -join "`n") -cne (($manifestPaths | Sort-Object) -join "`n") -or
            $resultLog -notlike "*summary_sha256=$($evidencePackage.summary_sha256)*" -or
            $resultLog -notlike "*evidence_manifest_sha256=$($evidencePackage.manifest_sha256)*") {
            throw "The Track M evidence package is incomplete, unsorted, or not closed by result.log."
        }
        $finalSourcePaths = [ordered]@{
            "scripts/run-realtime-gate.ps1" = $evidenceMain
            "scripts/run-realtime-gate-self-test.ps1" = $evidenceSelfTest
            "scripts/run-realtime-gate-summary.ps1" = $evidenceSummaryScript
        }
        $postCapturePath = Join-Path $evidenceResults `
            $evidenceSummary.workloads[0].candidate.runs[0].gate_artifacts.stdout_file
        $postCaptureBytes = [IO.File]::ReadAllBytes($postCapturePath)
        try {
            [IO.File]::WriteAllText($postCapturePath, "post-capture mutation`n")
            $postCaptureFailures = @(Get-TrackMEvidenceFinalVerificationFailures `
                $evidenceResults $manifest.result_directory_files `
                $manifest.gate_source_members $finalSourcePaths $manifest.workload_manifest `
                $evidenceFixtureManifest $evidencePackage.manifest_path `
                $evidencePackage.manifest_sha256 $evidencePackage.result_log_path `
                (Get-FileSha256 $evidencePackage.result_log_path))
            if (@($postCaptureFailures | Where-Object {
                $_ -like "*changed after manifest capture*"
            }).Count -eq 0) {
                throw "Final Track M verification accepted a post-capture raw artifact mutation."
            }
        } finally {
            [IO.File]::WriteAllBytes($postCapturePath, $postCaptureBytes)
        }

        $expectedProbe = [Collections.Generic.Dictionary[string, object]]::new(
            [StringComparer]::OrdinalIgnoreCase
        )
        $failureProbe = [Collections.Generic.List[string]]::new()
        Add-TrackMExpectedResultArtifact `
            $expectedProbe $failureProbe "probe.log" ("a" * 64) "probe" "first probe"
        Add-TrackMExpectedResultArtifact `
            $expectedProbe $failureProbe "probe.log" ("a" * 64) "probe" "duplicate probe"
        Add-TrackMExpectedResultArtifact `
            $expectedProbe $failureProbe "../escape.log" ("a" * 64) "probe" "traversal probe"
        if ($failureProbe.Count -ne 2) {
            throw "Track M evidence did not reject duplicate and traversal artifact names."
        }

        $generatedEvidence = @($evidencePackage.manifest_path, $evidencePackage.result_log_path)
        Remove-Item -LiteralPath $generatedEvidence -Force
        $tamperPath = Join-Path $evidenceResults `
            $evidenceSummary.workloads[0].candidate.runs[0].gate_artifacts.stdout_file
        $tamperBytes = [IO.File]::ReadAllBytes($tamperPath)
        try {
            [IO.File]::WriteAllText($tamperPath, "tampered`n")
            Assert-SelfTestThrows {
                Write-TrackMEvidencePackage `
                    $evidenceResults $evidenceSummaryPath $evidenceSummary `
                    $evidenceCandidate $evidenceParent $evidenceClosure `
                    $evidenceMain $evidenceSelfTest $evidenceSummaryScript $evidenceFixtureManifest
            } "integrity failed after packaging"
            if (-not (Test-Path -LiteralPath (Join-Path $evidenceResults "evidence-manifest.json")) -or
                -not (Test-Path -LiteralPath (Join-Path $evidenceResults "result.log"))) {
                throw "Failed Track M evidence was not packaged before rejection."
            }
        } finally {
            [IO.File]::WriteAllBytes($tamperPath, $tamperBytes)
        }
        Remove-Item -LiteralPath `
            (Join-Path $evidenceResults "evidence-manifest.json"), `
            (Join-Path $evidenceResults "result.log") -Force
        $missingPath = Join-Path $evidenceResults `
            $evidenceSummary.workloads[0].parent.runs[0].gate_artifacts.stderr_file
        $missingBytes = [IO.File]::ReadAllBytes($missingPath)
        Remove-Item -LiteralPath $missingPath -Force
        try {
            Assert-SelfTestThrows {
                Write-TrackMEvidencePackage `
                    $evidenceResults $evidenceSummaryPath $evidenceSummary `
                    $evidenceCandidate $evidenceParent $evidenceClosure `
                    $evidenceMain $evidenceSelfTest $evidenceSummaryScript $evidenceFixtureManifest
            } "missing result artifact"
        } finally {
            [IO.File]::WriteAllBytes($missingPath, $missingBytes)
        }
        Remove-Item -LiteralPath `
            (Join-Path $evidenceResults "evidence-manifest.json"), `
            (Join-Path $evidenceResults "result.log") -Force
        $unexpectedDirectory = Join-Path $evidenceResults "nested"
        New-Item -ItemType Directory -Path $unexpectedDirectory | Out-Null
        $unexpectedPath = Join-Path $unexpectedDirectory "unexpected.log"
        [IO.File]::WriteAllText($unexpectedPath, "unexpected`n")
        [IO.File]::SetAttributes($unexpectedPath, [IO.FileAttributes]::Hidden)
        Assert-SelfTestThrows {
            Write-TrackMEvidencePackage `
                $evidenceResults $evidenceSummaryPath $evidenceSummary `
                $evidenceCandidate $evidenceParent $evidenceClosure `
                $evidenceMain $evidenceSelfTest $evidenceSummaryScript $evidenceFixtureManifest
        } "unexpected result artifact"
    } finally {
        Remove-Item -LiteralPath $evidenceRoot -Recurse -Force
    }

    $directEvidenceRoot = Join-Path ([IO.Path]::GetTempPath()) (
        "izarravm-direct-evidence-$([guid]::NewGuid().ToString('N'))"
    )
    $directEvidenceResults = Join-Path $directEvidenceRoot "results"
    $directEvidenceSources = Join-Path $directEvidenceRoot "sources"
    New-Item -ItemType Directory -Path $directEvidenceResults, $directEvidenceSources |
        Out-Null
    try {
        $directCandidatePath = Join-Path $directEvidenceResults "candidate-izarravm.exe"
        $directParentPath = Join-Path $directEvidenceResults "parent-izarravm.exe"
        [IO.File]::WriteAllText($directCandidatePath, "candidate executable`n")
        [IO.File]::WriteAllText($directParentPath, "parent executable`n")
        $directEvidenceCandidate = [pscustomobject]@{
            executed_copy_path = $directCandidatePath
            sha256 = Get-FileSha256 $directCandidatePath
        }
        $directEvidenceParent = [pscustomobject]@{
            executed_copy_path = $directParentPath
            sha256 = Get-FileSha256 $directParentPath
        }
        $directEvidenceSummary = [pscustomobject][ordered]@{
            schema = "izarravm-direct-quake-campaign-partial-proof-v1"
            stage = "proof"
            verdict = "normal_promotion_threshold_met"
            retention_eligible = $false
            revision_pair = [pscustomobject]@{
                candidate_commit = $revision
                candidate_tree = $candidateTree
                parent_commit = $baselineCommit
                parent_tree = $baselineTree
            }
            workload_manifest_sha256 = [pscustomobject]@{ at_completion = $null }
            workloads = [object[]]@($directWorkload)
        }
        $directSamples = [object[]]@(
            $directWorkload.observation_classes.correctness.candidate,
            $directWorkload.observation_classes.correctness.parent,
            $directWorkload.discarded_warmups.candidate[0],
            $directWorkload.discarded_warmups.parent[0]
        ) + [object[]]@($directWorkload.candidate.runs) +
            [object[]]@($directWorkload.parent.runs)
        foreach ($sample in $directSamples) {
            foreach ($name in @("profile_json", "stdout", "stderr", "qconsole", "hdd_tree")) {
                $fileProperty = "${name}_file"
                $hashProperty = "${name}_sha256"
                $path = Join-Path $directEvidenceResults $sample.gate_artifacts.$fileProperty
                [IO.File]::WriteAllText(
                    $path,
                    "$($sample.gate_role) $($sample.gate_observation) $name`n"
                )
                $sample.gate_artifacts.$hashProperty = Get-FileSha256 $path
            }
        }
        $directEvidenceMain = Join-Path $directEvidenceSources "run-realtime-gate.ps1"
        $directEvidenceSelfTest = Join-Path $directEvidenceSources `
            "run-realtime-gate-self-test.ps1"
        $directEvidenceSummaryScript = Join-Path $directEvidenceSources `
            "run-realtime-gate-summary.ps1"
        Copy-Item -LiteralPath $gateMainScriptPath -Destination $directEvidenceMain
        Copy-Item -LiteralPath $gateSelfTestScriptPath -Destination $directEvidenceSelfTest
        Copy-Item -LiteralPath $gateSummaryScriptPath -Destination $directEvidenceSummaryScript
        $directEvidenceClosure = Get-GateSourceClosureIdentity `
            $directEvidenceMain $directEvidenceSelfTest $directEvidenceSummaryScript
        $directEvidenceFixtureManifest = Join-Path $directEvidenceSources `
            "realtime-gate-inputs.json"
        [IO.File]::WriteAllText(
            $directEvidenceFixtureManifest,
            "{`"schema`":`"self-test`"}`n"
        )
        $directEvidenceSummary.workload_manifest_sha256.at_completion =
            Get-FileSha256 $directEvidenceFixtureManifest
        $directEvidenceSummaryPath = Join-Path $directEvidenceResults "summary.json"
        $directEvidenceSummary | ConvertTo-Json -Depth 20 |
            Set-Content -LiteralPath $directEvidenceSummaryPath -Encoding utf8
        $directPackage = Write-DirectQuakeCampaignEvidencePackage `
            $directEvidenceResults $directEvidenceSummaryPath $directEvidenceSummary `
            $directEvidenceCandidate $directEvidenceParent $directEvidenceClosure `
            $directEvidenceMain $directEvidenceSelfTest $directEvidenceSummaryScript `
            $directEvidenceFixtureManifest
        $directManifest = Get-Content -LiteralPath $directPackage.manifest_path -Raw |
            ConvertFrom-Json
        $directResultLog = Get-Content -LiteralPath $directPackage.result_log_path -Raw
        if ($directManifest.schema -cne
                "izarravm-direct-quake-campaign-evidence-manifest-v1" -or
            $directManifest.integrity_verdict -cne "pass" -or
            $directManifest.retention_eligible -or
            $directManifest.result_directory_files.Count -ne 83 -or
            $directResultLog -notlike "*retention_eligible=false*") {
            throw "The Direct Quake evidence package is incomplete or retainable."
        }
        Remove-Item -LiteralPath $directPackage.manifest_path, $directPackage.result_log_path -Force
        $directTamperPath = Join-Path $directEvidenceResults `
            $directWorkload.candidate.runs[0].gate_artifacts.hdd_tree_file
        [IO.File]::WriteAllText($directTamperPath, "tampered HDD manifest`n")
        Assert-SelfTestThrows {
            Write-DirectQuakeCampaignEvidencePackage `
                $directEvidenceResults $directEvidenceSummaryPath $directEvidenceSummary `
                $directEvidenceCandidate $directEvidenceParent $directEvidenceClosure `
                $directEvidenceMain $directEvidenceSelfTest $directEvidenceSummaryScript `
                $directEvidenceFixtureManifest
        } "evidence integrity failed"
    } finally {
        Remove-Item -LiteralPath $directEvidenceRoot -Recurse -Force
    }

    $pollPolicy = Get-WorkloadPolicy "doom-586"
    $newPollSample = {
        param(
            [string]$Role,
            [string]$Observation,
            [double]$RealTimeFactor
        )
        $enabled = $Role -ceq "skip_on"
        $instructions = if ($enabled) { [uint64]900 } else { [uint64]1000 }
        return [pscustomobject][ordered]@{
            wall_seconds = 10.0
            guest_seconds = 10.0 * $RealTimeFactor
            real_time_factor = $RealTimeFactor
            instructions_per_host_second = $instructions / 10.0
            direct_native_coverage = 0.0
            direct_slow_exits_per_100_instructions = 0.0
            perf = [pscustomobject][ordered]@{
                instructions = $instructions
                jit_region_entries = 0
                jit_region_insns = 0
                jit_native_insns = 0
                jit_direct_entries = 0
                jit_direct_insns = 0
                jit_direct_side_exits = 0
                poll_skip_spans = if ($enabled) { [uint64]5 } else { [uint64]0 }
                poll_skip_iterations = if ($enabled) { [uint64]100 } else { [uint64]0 }
            }
            master_ticks = [uint64]2000
            elapsed_budget_clocks = [uint64]3000
            executed_cpu_core_clocks = [uint64]1100
            raw_bus_clocks = [uint64]1900
            stop = [pscustomobject]@{ kind = "test_exit"; code = 0 }
            timedemo = [pscustomobject]@{ gametics = 2134; realtics = 843 }
            gate_process_exit_code = 0
            gate_role = $Role
            gate_observation = $Observation
            gate_processor_index = 8
            gate_processor_affinity_mask = "0x0000000000000100"
            gate_processor_affinity_verified = $true
            gate_execution_cli = "--interpreter"
            gate_execution_jit = "0"
            gate_poll_skip = if ($enabled) { "1" } else { "0" }
            gate_measurement_fixture_sha256 = "c" * 64
            gate_termination_policy = "lotura_test_exit"
            gate_fixture = $null
            gate_artifacts = [pscustomobject][ordered]@{
                profile_json_file = "doom-586-$Role-$Observation.json"
                profile_json_sha256 = "4" * 64
                stdout_file = "doom-586-$Role-$Observation.stdout.log"
                stdout_sha256 = "5" * 64
                stderr_file = "doom-586-$Role-$Observation.stderr.log"
                stderr_sha256 = "6" * 64
                qconsole_file = $null
                qconsole_sha256 = $null
                result_block_status = "valid"
                result_block_count = 1
                result_block_sha256 = "7" * 64
                result_block_normalized_bytes = 128
            }
        }
    }
    $newPollComparison = {
        param([double[]]$Ratios)
        $skipOn = @()
        $skipOff = @()
        foreach ($pair in 1..$Ratios.Count) {
            $skipOn += & $newPollSample "skip_on" "pair$pair" $Ratios[$pair - 1]
            $skipOff += & $newPollSample "skip_off" "pair$pair" 1.0
        }
        return [pscustomobject][ordered]@{
            skip_on = [object[]]$skipOn
            skip_off = [object[]]$skipOff
            warmups = [pscustomobject][ordered]@{
                skip_off = [object[]]@(& $newPollSample "skip_off" "warmup" 1.0)
                skip_on = [object[]]@(& $newPollSample "skip_on" "warmup" 1.0)
            }
        }
    }
    $pollComparison = & $newPollComparison `
        ([double[]](1.01, 1.01, 1.01, 1.01, 1.01, 1.01))
    $pollWorkload = Get-PollSkipWorkloadSummary `
        $pollPolicy $pollComparison.skip_on $pollComparison.skip_off $pollComparison.warmups
    if (-not $pollWorkload.valid_performance_result -or
        $pollWorkload.verdicts.performance -cne "improved" -or
        $pollWorkload.poll_counters.stable_instruction_reduction -ne 100 -or
        $null -ne $pollWorkload.diagnostic_metrics.instructions_per_host_second.PSObject.Properties["verdict"]) {
        throw "A valid POLL-SKIP comparison was rejected or graded by IPS."
    }
    Assert-PollSkipSample $pollComparison.warmups.skip_off[0] `
        "skip_off" "warmup" $pollPolicy
    Assert-PollSkipSample $pollComparison.warmups.skip_on[0] `
        "skip_on" "warmup" $pollPolicy
    if ((Assert-PollSkipPair `
        $pollPolicy.name $pollComparison.warmups.skip_on[0] `
        $pollComparison.warmups.skip_off[0] "warmup") -ne 100) {
        throw "POLL-SKIP warmup instruction reduction is wrong."
    }

    $sampleFailureCases = [ordered]@{
        anchor = { param($sample) $sample.timedemo.realtics = 842 }
        result = { param($sample) $sample.gate_artifacts.result_block_status = "invalid" }
        jit = { param($sample) $sample.perf.jit_direct_entries = 1 }
        fixture = { param($sample) $sample.gate_measurement_fixture_sha256 = $null }
        affinity = { param($sample) $sample.gate_processor_index = 7 }
        execution = { param($sample) $sample.gate_execution_cli = "automatic" }
        skip_off_counter = { param($sample) $sample.perf.poll_skip_spans = 1 }
    }
    foreach ($case in $sampleFailureCases.GetEnumerator()) {
        $sample = $pollComparison.warmups.skip_off[0] |
            ConvertTo-Json -Depth 12 | ConvertFrom-Json
        & $case.Value $sample
        Assert-SelfTestThrows {
            Assert-PollSkipSample $sample "skip_off" "warmup" $pollPolicy
        } "failed"
    }
    $disabledOnSample = $pollComparison.warmups.skip_on[0] |
        ConvertTo-Json -Depth 12 | ConvertFrom-Json
    $disabledOnSample.perf.poll_skip_iterations = 0
    Assert-SelfTestThrows {
        Assert-PollSkipSample $disabledOnSample "skip_on" "warmup" $pollPolicy
    } "positive POLL-SKIP counters"
    foreach ($field in @("instructions", "poll_skip_spans", "poll_skip_iterations")) {
        $drift = $pollComparison.skip_on[0] |
            ConvertTo-Json -Depth 12 | ConvertFrom-Json
        $drift.perf.$field++
        Assert-SelfTestThrows {
            Assert-PollSkipRoleReference `
                $pollPolicy.name "skip_on" $pollComparison.warmups.skip_on[0] $drift
        } $field
    }
    $timingDrift = $pollComparison.skip_on[0] |
        ConvertTo-Json -Depth 12 | ConvertFrom-Json
    $timingDrift.raw_bus_clocks++
    Assert-SelfTestThrows {
        Assert-PollSkipRoleReference `
            $pollPolicy.name "skip_on" $pollComparison.warmups.skip_on[0] $timingDrift
    } "raw_bus_clocks"
    $noReduction = $pollComparison.skip_on[0] |
        ConvertTo-Json -Depth 12 | ConvertFrom-Json
    $noReduction.perf.instructions = $pollComparison.skip_off[0].perf.instructions
    Assert-SelfTestThrows {
        Assert-PollSkipPair $pollPolicy.name $noReduction $pollComparison.skip_off[0] "pair 1"
    } "positive instruction reduction"

    $Runs = 6
    $policies = @($pollPolicy)
    $pairRoles = @("skip_on", "skip_off")
    $pollSkipExecutionPolicies = [ordered]@{
        skip_off = $skipOffPolicy
        skip_on = $skipOnPolicy
    }
    $diagnosticVariables = [string[]](Get-KnownDiagnosticVariables)
    $verifiedChildAffinityMasks = [Collections.Generic.List[string]]::new()
    foreach ($child in 1..(2 + 2 * $Runs)) {
        $verifiedChildAffinityMasks.Add("0x0000000000000100")
    }
    $verifiedChildAffinityStable = $true
    $fixtureManifestMatches = [ordered]@{
        doom = [ordered]@{
            preflight_required_inputs = $true
            preflight_canonical_tree = $true
            frozen_required_inputs = $true
            frozen_canonical_tree = $true
        }
    }
    $workloadInputHashes = [ordered]@{
        doom_586 = [ordered]@{ "AUTOEXEC.BAT" = "a" * 64 }
    }
    $workloadTreeHashes = [ordered]@{ doom = "b" * 64 }
    $workloadCanonicalTreeHashes = [ordered]@{ doom = "c" * 64 }
    $pollSkipPowerSchemeEligible = $true
    $pollSummary = New-PollSkipComparisonSummary @($pollWorkload)
    if ($pollSummary.schema -cne "izarravm-poll-skip-comparison-v1" -or
        $pollSummary.verdict -cne "improved" -or -not $pollSummary.valid_performance_result -or
        -not $pollSummary.role_executables.same_executable -or
        $pollSummary.role_executables.skip_on.path -cne $pollSummary.role_executables.skip_off.path -or
        $pollSummary.role_executables.skip_on.sha256 -cne $pollSummary.role_executables.skip_off.sha256 -or
        ($pollSummary.warmup_order -join ",") -cne "skip_off,skip_on" -or
        $pollSummary.execution.diagnostics_unset -contains "IZARRAVM_JIT" -or
        $pollSummary.execution.diagnostics_unset -contains "IZARRAVM_POLL_SKIP" -or
        $pollSummary.execution.diagnostics_unset -notcontains "IZARRAVM_POLL_SKIP_DIAG" -or
        $pollSummary.execution.diagnostics_unset -notcontains "IZARRAVM_UNIT_SIM" -or
        $pollSummary.acceptance.ips_is_diagnostic_only -ne $true) {
        throw "The POLL-SKIP top-level proof summary is incomplete."
    }
    $sixPairRouting = $pollWorkload | ConvertTo-Json -Depth 20 | ConvertFrom-Json
    $sixPairRouting.paired_metrics.real_time_factor.verdict = "positive_but_inconclusive"
    $sixPairRouting.paired_metrics.real_time_factor.classification = "positive_but_inconclusive"
    $sixPairRouting.paired_metrics.real_time_factor.twelve_pair_confirmation_required = $true
    $sixPairRouting.verdicts.performance = "positive_but_inconclusive"
    $sixPairRouting.checks.performance.verdict = "positive_but_inconclusive"
    $sixPairSummary = New-PollSkipComparisonSummary @($sixPairRouting)
    if ($sixPairSummary.verdict -cne "positive_but_inconclusive" -or
        -not $sixPairSummary.twelve_pair_confirmation_required) {
        throw "A positive six-pair POLL-SKIP result did not request confirmation."
    }
    $Runs = 12
    $verifiedChildAffinityMasks = [Collections.Generic.List[string]]::new()
    foreach ($child in 1..(2 + 2 * $Runs)) {
        $verifiedChildAffinityMasks.Add("0x0000000000000100")
    }
    $twelvePairRouting = $pollWorkload | ConvertTo-Json -Depth 20 | ConvertFrom-Json
    $twelvePairRouting.paired_metrics.real_time_factor.verdict = "speedup_not_demonstrated"
    $twelvePairRouting.paired_metrics.real_time_factor.classification = "neutral"
    $twelvePairRouting.paired_metrics.real_time_factor.twelve_pair_confirmation_required = $false
    $twelvePairRouting.verdicts.performance = "speedup_not_demonstrated"
    $twelvePairRouting.checks.performance.verdict = "speedup_not_demonstrated"
    $twelvePairSummary = New-PollSkipComparisonSummary @($twelvePairRouting)
    if ($twelvePairSummary.verdict -cne "speedup_not_demonstrated" -or
        $twelvePairSummary.twelve_pair_confirmation_required) {
        throw "A non-improved twelve-pair result was not marked speedup_not_demonstrated."
    }
    $Runs = 6
    $verifiedChildAffinityMasks = [Collections.Generic.List[string]]::new()
    foreach ($child in 1..(2 + 2 * $Runs)) {
        $verifiedChildAffinityMasks.Add("0x0000000000000100")
    }
    $pollGlobalProvenanceCases = [ordered]@{
        executable = [pscustomobject]@{
            mutate = { $candidateArtifact.verified = $false }
            restore = { $candidateArtifact.verified = $true }
        }
        repository = [pscustomobject]@{
            mutate = { $repositoryAtSelection.dirty = $true }
            restore = { $repositoryAtSelection.dirty = $false }
        }
        fixture = [pscustomobject]@{
            mutate = { $doomFrozenStable = $false }
            restore = { $doomFrozenStable = $true }
        }
        affinity = [pscustomobject]@{
            mutate = { $verifiedChildAffinityStable = $false }
            restore = { $verifiedChildAffinityStable = $true }
        }
        power = [pscustomobject]@{
            mutate = { $pollSkipPowerSchemeEligible = $false }
            restore = { $pollSkipPowerSchemeEligible = $true }
        }
        lock = [pscustomobject]@{
            mutate = { $measurementLockEvidence.path = "C:\wrong.lock" }
            restore = { $measurementLockEvidence.path = $MeasurementLockPath }
        }
        source_closure = [pscustomobject]@{
            mutate = { $gateSourceClosureStable = $false }
            restore = { $gateSourceClosureStable = $true }
        }
        manifest = [pscustomobject]@{
            mutate = { $fixtureManifestStable = $false }
            restore = { $fixtureManifestStable = $true }
        }
        canonical_hash = [pscustomobject]@{
            mutate = { $workloadCanonicalTreeHashes.doom = "wrong" }
            restore = { $workloadCanonicalTreeHashes.doom = "c" * 64 }
        }
    }
    foreach ($case in $pollGlobalProvenanceCases.GetEnumerator()) {
        try {
            . $case.Value.mutate
            $invalidPollSummary = New-PollSkipComparisonSummary @($pollWorkload)
            if ($invalidPollSummary.verdict -cne "invalid" -or
                $invalidPollSummary.valid_performance_result) {
                throw "POLL-SKIP global provenance mutation $($case.Key) was accepted."
            }
        } finally {
            . $case.Value.restore
        }
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
