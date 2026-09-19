#Requires -Version 5.1
<#
.SYNOPSIS
    内存故障诊断（只读）。

.DESCRIPTION
    本机反复蓝屏、编译工具随机崩、文件被写坏时，先用它确认「是不是内存」。

    它只做三件事：
      1. 读系统日志 —— 蓝屏记录、硬件错误上报、以往的内存诊断结果
      2. 读 WMI    —— 内存条数量 / 型号 / 频率、主板、机型、CPU
      3. 把结论与下一步写成一份报告

    它 **不修改任何系统设置、不重启、不删除任何文件**。
    报告同时打印到屏幕，并写入脚本所在目录的 mem-diag-<时间戳>.txt。

.PARAMETER Days
    回看最近多少天的系统日志。默认 30。

.PARAMETER OutDir
    报告输出目录。默认脚本所在目录。

.EXAMPLE
    mem-diag.cmd

.EXAMPLE
    mem-diag.cmd -Days 7
#>
[CmdletBinding()]
param(
    [int]    $Days   = 30,
    [string] $OutDir = $PSScriptRoot
)

$ErrorActionPreference = 'Continue'

# ─────────────────────────────────────────────────────────────────────
# 输出：屏幕 + 内存缓冲，最后统一落盘
#   （写文件失败也不能让整份诊断白跑，所以先攒着、finally 里落盘）
# ─────────────────────────────────────────────────────────────────────
$script:Log = New-Object System.Text.StringBuilder

function Say {
    param([string]$Text = '')
    Write-Host $Text
    [void]$script:Log.AppendLine($Text)
}

function Head {
    param([string]$Title)
    Say ''
    Say ('=' * 72)
    Say $Title
    Say ('=' * 72)
}

# ─────────────────────────────────────────────────────────────────────
# bugcheck 主码 → 名称 + 是否常见于内存/硬件故障
#   依据：微软 Bug Check Code Reference 里各码的 Cause 段。
#   ★ 只列常见的。表里没有的码照样原样显示 —— **不猜名称、不猜倾向**。
#   ★ 判据只有这一处：下面的分类与统计都从这里派生。
# ─────────────────────────────────────────────────────────────────────
$BugCheckTable = @{
    0x0A  = @{ Name = 'IRQL_NOT_LESS_OR_EQUAL';                  Memory = $true  }
    0x19  = @{ Name = 'BAD_POOL_HEADER';                         Memory = $true  }
    0x1A  = @{ Name = 'MEMORY_MANAGEMENT';                       Memory = $true  }
    0x1E  = @{ Name = 'KMODE_EXCEPTION_NOT_HANDLED';             Memory = $false }
    0x34  = @{ Name = 'CACHE_MANAGER';                           Memory = $true  }
    0x3B  = @{ Name = 'SYSTEM_SERVICE_EXCEPTION';                Memory = $false }
    0x4D  = @{ Name = 'PAGE_FAULT_WITH_INTERRUPTS_OFF';          Memory = $true  }
    0x4E  = @{ Name = 'PFN_LIST_CORRUPT';                        Memory = $true  }
    0x50  = @{ Name = 'PAGE_FAULT_IN_NONPAGED_AREA';             Memory = $true  }
    0x7A  = @{ Name = 'KERNEL_DATA_INPAGE_ERROR';                Memory = $true  }
    0x7E  = @{ Name = 'SYSTEM_THREAD_EXCEPTION_NOT_HANDLED';     Memory = $false }
    0x7F  = @{ Name = 'UNEXPECTED_KERNEL_MODE_TRAP';             Memory = $true  }
    0x9C  = @{ Name = 'MACHINE_CHECK_EXCEPTION';                 Memory = $true  }
    0xC1  = @{ Name = 'SPECIAL_POOL_DETECTED_MEMORY_CORRUPTION'; Memory = $true  }
    0xC2  = @{ Name = 'BAD_POOL_CALLER';                         Memory = $true  }
    0xC4  = @{ Name = 'DRIVER_VERIFIER_DETECTED_VIOLATION';      Memory = $false }
    0xC5  = @{ Name = 'DRIVER_CORRUPTED_EXPOOL';                 Memory = $true  }
    0xD1  = @{ Name = 'DRIVER_IRQL_NOT_LESS_OR_EQUAL';           Memory = $false }
    0xEF  = @{ Name = 'CRITICAL_PROCESS_DIED';                   Memory = $false }
    0x109 = @{ Name = 'CRITICAL_STRUCTURE_CORRUPTION';           Memory = $true  }
    0x124 = @{ Name = 'WHEA_UNCORRECTABLE_ERROR';                Memory = $true  }
    0x139 = @{ Name = 'KERNEL_SECURITY_CHECK_FAILURE';           Memory = $false }
    0x12B = @{ Name = 'FAULTY_HARDWARE_CORRUPTED_PAGE';          Memory = $true  }
    0x13A = @{ Name = 'KERNEL_MODE_HEAP_CORRUPTION';             Memory = $true  }
}

# Windows 的固定事件契约（不是可调参数，故就地写死）：
#   1001 @ WER-SystemErrorReporting = 带 bugcheck 码的那一条
#   41   @ Kernel-Power             = 「上次关机不正常」——是**结果**，不带原因
$EVENT_BUGCHECK  = 1001
$EVENT_DIRTY_OFF = 41
$PROVIDER_BUGCHECK = 'Microsoft-Windows-WER-SystemErrorReporting'

# DDR4 的 JEDEC 原生档最高到 2666 MHz。跑得比它高，必然是 XMP / DOCP 这类超频档 ——
# 这条判据用来决定「要不要建议去 BIOS 关超频」，写死是因为它是 DDR4 的行业事实，
# 不是本机可调参数。
$JEDEC_MAX_MHZ = 2666
$anyOverclocked = $false

# ─────────────────────────────────────────────────────────────────────
# 事件读取：日志不可用 / 无权限 / 无记录时一律返回空数组，绝不中断
# ─────────────────────────────────────────────────────────────────────
function Get-Events {
    param([hashtable]$Filter, [int]$Max = 300)
    try {
        return @(Get-WinEvent -FilterHashtable $Filter -MaxEvents $Max -ErrorAction Stop)
    } catch {
        return @()
    }
}

# 从 1001 的正文里取主码与 4 个参数。
#   ★ 不按自然语言匹配（系统语言不同、正文就不同），只认 "0x........ (…)" 这个格式，
#     它是跨语言固定的。
function Parse-BugCheck {
    param([string]$Message)
    if ($Message -match '0x([0-9A-Fa-f]{8})\s*\(([^)]*)\)') {
        $parms = @()
        if ($matches[2].Trim()) {
            $parms = @($matches[2] -split ',' | ForEach-Object { $_.Trim() })
        }
        return [pscustomobject]@{ Code = [Convert]::ToInt32($matches[1], 16); Parms = $parms }
    }
    return $null
}

# 表里查不到就说不知道，不编
function Describe-BugCheck {
    param([int]$Code)
    if ($BugCheckTable.ContainsKey($Code)) { return $BugCheckTable[$Code] }
    return @{ Name = '(未收录)'; Memory = $null }
}

# ─────────────────────────────────────────────────────────────────────
# 采集（先全部拿到手，再决定怎么说）
# ─────────────────────────────────────────────────────────────────────
$since = (Get-Date).AddDays(-$Days)

# ★ 每个调用点都套 @() —— PowerShell 的函数返回**单元素数组会被拆成标量**，
#   套一层 @() 才能保证 .Count / -ge 2 这类判断拿到的真是数组。
$bugEvents = @(Get-Events -Filter @{
    LogName      = 'System'
    ProviderName = $PROVIDER_BUGCHECK
    Id           = $EVENT_BUGCHECK
    StartTime    = $since
})
$dirtyOffCount = @(Get-Events -Filter @{
    LogName   = 'System'
    Id        = $EVENT_DIRTY_OFF
    StartTime = $since
}).Count

$wheaEvents = @(Get-Events -Filter @{
    LogName      = 'System'
    ProviderName = 'Microsoft-Windows-WHEA-Logger'
    StartTime    = $since
})

$memTestEvents = @(Get-Events -Filter @{
    ProviderName = 'Microsoft-Windows-MemoryDiagnostics-Results'
} -Max 10)

$crashes = @()
foreach ($e in $bugEvents) {
    $parsed = Parse-BugCheck -Message $e.Message
    if ($parsed) {
        $info = Describe-BugCheck -Code $parsed.Code
        $crashes += [pscustomobject]@{
            Time   = $e.TimeCreated
            Code   = $parsed.Code
            Name   = $info.Name
            Memory = $info.Memory
            Parms  = $parsed.Parms
        }
    }
}
$crashes = @($crashes | Sort-Object Time -Descending)

function Get-CimSafe {
    param([string]$Class)
    try { return @(Get-CimInstance $Class -ErrorAction Stop) } catch { return @() }
}

$dimms    = @(Get-CimSafe 'Win32_PhysicalMemory')
$os       = (@(Get-CimSafe 'Win32_OperatingSystem') | Select-Object -First 1)
$cs       = (@(Get-CimSafe 'Win32_ComputerSystem')  | Select-Object -First 1)
$board    = (@(Get-CimSafe 'Win32_BaseBoard')       | Select-Object -First 1)
$cpu      = (@(Get-CimSafe 'Win32_Processor')       | Select-Object -First 1)

$sysRoot  = $env:SystemRoot
$dumpFull = Join-Path $sysRoot 'MEMORY.DMP'
$dumpDir  = Join-Path $sysRoot 'Minidump'

# 机器名不要用 $env:COMPUTERNAME —— 环境变量在某些宿主里会被剥离成空串，
# 走 Win32 API 才稳。
$machineName = [System.Environment]::MachineName
# 内存容量从系统读，不写死 —— 它下面要用来估内存诊断的耗时。
$totalGb = 0
if ($os) { $totalGb = [math]::Round($os.TotalVisibleMemorySize / 1MB, 1) }

# ─────────────────────────────────────────────────────────────────────
# 出报告
# ─────────────────────────────────────────────────────────────────────
if ([string]::IsNullOrWhiteSpace($OutDir)) { $OutDir = (Get-Location).Path }
if (-not (Test-Path -LiteralPath $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
}
$report = Join-Path $OutDir ('mem-diag-{0}.txt' -f (Get-Date -Format 'yyyyMMdd-HHmmss'))

try {
    Say ''
    Say '  内存故障诊断（只读）'
    Say ('  主机 {0}    时间 {1}' -f $machineName, (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'))
    Say ('  回看最近 {0} 天' -f $Days)

    # ── 1. 蓝屏记录 ──────────────────────────────────────────────────
    Head "1. 蓝屏记录（事件 $EVENT_BUGCHECK，近 $Days 天）"
    if ($crashes.Count -eq 0) {
        Say '  没有记录。'
        Say '  若崩溃仍在发生，说明它没留下 bugcheck —— 更可能是应用层/驱动问题，不是内存。'
    } else {
        Say ('  共 {0} 次：' -f $crashes.Count)
        Say ''
        foreach ($c in $crashes) {
            $mark = '   '
            if ($c.Memory -eq $true) { $mark = ' ★ ' }
            Say ('{0}{1}  0x{2:X2}  {3}' -f $mark, $c.Time.ToString('MM-dd HH:mm:ss'), $c.Code, $c.Name)
            if ($c.Parms.Count -gt 0) {
                Say ('       参数: ' + ($c.Parms -join ', '))
            }
        }
        Say ''
        Say '  ★ = 该码常见于内存 / 硬件故障（依据见脚本内 $BugCheckTable）'
    }
    Say ('  另有「上次关机不正常」事件（Id {0}）{1} 次 —— 那是结果，不是原因。' -f $EVENT_DIRTY_OFF, $dirtyOffCount)

    # ── 2. 按码统计 ──────────────────────────────────────────────────
    Head '2. 按码统计'
    if ($crashes.Count -eq 0) {
        Say '  （无）'
    } else {
        $groups = $crashes | Group-Object Code | Sort-Object Count -Descending
        foreach ($g in $groups) {
            $code = [int]$g.Name
            $info = Describe-BugCheck -Code $code
            $tag  = ''
            if ($info.Memory -eq $true)  { $tag = '   ★ 内存/硬件类' }
            if ($info.Memory -eq $false) { $tag = '   （非内存类典型码）' }
            if ($null -eq $info.Memory)  { $tag = '   （未收录，不作判断）' }
            Say ('  {0,3} 次   0x{1:X2}  {2}{3}' -f $g.Count, $code, $info.Name, $tag)
        }
        $memCount = @($crashes | Where-Object { $_.Memory -eq $true }).Count
        Say ''
        Say ('  ⇒ {0} 次里有 {1} 次是内存/硬件类码。' -f $crashes.Count, $memCount)
        if ($memCount -ge 2) {
            Say '     多次重复出现 ⇒ 强烈指向内存或供电/主板，优先按内存查。'
        } elseif ($memCount -ge 1) {
            Say '     出现过 ⇒ 值得按内存查，但一次不足以定案。'
        } else {
            Say '     一次都没有 ⇒ 不能只怪内存，先看转储（见第 6 节）。'
        }

        # 同一个码反复出现时，再看第二个参数（多数码里它是出错地址）落点是否固定。
        # ★ 这是**辅助判断**，不是结论：地址随机 ⇒ 更像随机损坏；
        #   地址固定 ⇒ 更像某个模块的固定缺陷。两者都不排除内存。
        $repeated = @($groups | Where-Object { $_.Count -ge 2 })
        if ($repeated.Count -gt 0) {
            Say ''
            Say '  同一个码反复出现时，看第二个参数（多数码里它是出错地址）落在哪儿：'
            foreach ($g in $repeated) {
                $code  = [int]$g.Name
                $addrs = @($g.Group | ForEach-Object {
                    if ($_.Parms.Count -ge 2) { $_.Parms[1] } else { '(无)' }
                } | Sort-Object -Unique)
                $verdict = '各不相同 ⇒ 随机位置被损坏，更像内存/供电'
                if ($addrs.Count -eq 1) {
                    $verdict = '每次同一个地址 ⇒ 更像某个模块的固定缺陷'
                }
                Say ('    0x{0:X2}  出现 {1} 次，{2} 个不同地址 —— {3}' -f $code, $g.Count, $addrs.Count, $verdict)
            }
        }
    }

    # ── 3. 硬件错误上报 ──────────────────────────────────────────────
    Head '3. 硬件错误上报（WHEA-Logger）'
    if ($wheaEvents.Count -eq 0) {
        Say '  无记录。'
        Say '  ⚠ 这**不能**排除硬件问题：WHEA 只在错误能被硬件上报时才记日志。'
        Say '    普通非 ECC 内存的位翻转，多数情况下根本不会产生 WHEA 事件。'
    } else {
        foreach ($e in $wheaEvents) {
            Say ('  {0}  Id={1}  {2}' -f $e.TimeCreated, $e.Id, $e.LevelDisplayName)
            $line = ($e.Message -split "`r?`n" | Where-Object { $_.Trim() } | Select-Object -First 1)
            if ($line) { Say ('       ' + $line.Trim()) }
        }
    }

    # ── 4. 以往的内存诊断结果 ────────────────────────────────────────
    Head '4. 以往的内存诊断结果'
    if ($memTestEvents.Count -eq 0) {
        Say '  从没跑过 Windows 内存诊断（没有结果记录）。'
    } else {
        foreach ($e in $memTestEvents) {
            Say ('  {0}  Id={1}  {2}' -f $e.TimeCreated, $e.Id, $e.LevelDisplayName)
        }
    }

    # ── 5. 内存硬件清单 ──────────────────────────────────────────────
    Head '5. 内存硬件清单'
    if ($dimms.Count -eq 0) {
        Say '  读不到内存条信息（可能需要在管理员权限下运行）。'
    } else {
        Say ('  共 {0} 条：' -f $dimms.Count)
        foreach ($d in $dimms) {
            Say ''
            Say ('    槽位   {0}' -f $d.DeviceLocator)
            Say ('    容量   {0} GB' -f [math]::Round($d.Capacity / 1GB, 1))
            $rated = $d.Speed
            $run   = $d.ConfiguredClockSpeed
            Say ('    频率   额定 {0} MHz / 实际运行 {1} MHz' -f $rated, $run)
            Say ('    厂商   {0}' -f $d.Manufacturer)
            Say ('    型号   {0}' -f $d.PartNumber)
            Say ('    序列号 {0}' -f $d.SerialNumber)
            if ($run -gt $JEDEC_MAX_MHZ) { $anyOverclocked = $true }
            if ($run -and $rated -and $run -lt $rated) {
                Say '    ⚠ 没跑在内存条的额定档 —— 说明 BIOS 里没启用 XMP / 超频档，用的是默认档。'
                Say '      **这本身不是故障**：CPU 内存控制器的原生上限也在这里起作用'
                Say '      （例如 Skylake 原生只到 DDR4-2133）。'
                Say '      ⇒「跑不到额定频率」不能当作内存损坏的证据。'
            }
        }
    }
    Say ''
    Say ('  CPU    {0}' -f $cpu.Name)
    Say ('  主板   {0} {1}' -f $board.Manufacturer, $board.Product)
    Say ('  机型   {0} {1}' -f $cs.Manufacturer, $cs.Model)
    Say ('  系统   {0} (build {1})' -f $os.Caption, $os.BuildNumber)
    Say ('  内存   共 {0} GB' -f $totalGb)

    # ── 6. 转储文件 ──────────────────────────────────────────────────
    Head '6. 转储文件（可用于定位到具体模块）'
    if (Test-Path -LiteralPath $dumpFull) {
        $f = Get-Item -LiteralPath $dumpFull
        Say ('  完整内核转储  {0}   {1:N0} MB' -f $f.LastWriteTime, ($f.Length / 1MB))
        Say ('  {0}' -f $dumpFull)
        Say '  ★ 每次蓝屏会覆盖同一个文件，所以它只对应最近一次。'
    } else {
        Say '  没有完整内核转储。'
    }
    if (Test-Path -LiteralPath $dumpDir) {
        $mini = @(Get-ChildItem -LiteralPath $dumpDir -Filter *.dmp -ErrorAction SilentlyContinue |
                  Sort-Object LastWriteTime -Descending)
        if ($mini.Count -eq 0) {
            Say ('  {0} 存在，但里面没有 .dmp —— 本机只保留了完整转储。' -f $dumpDir)
        } else {
            Say ('  小转储 {0} 个（最近 5 个）：' -f $mini.Count)
            foreach ($m in ($mini | Select-Object -First 5)) {
                Say ('    {0}   {1:N1} MB   {2}' -f $m.LastWriteTime, ($m.Length / 1MB), $m.Name)
            }
        }
    } else {
        Say ('  没有小转储目录 {0}。' -f $dumpDir)
    }

    # ── 7. 下一步 ────────────────────────────────────────────────────
    Head '7. 下一步（按顺序做）'
    Say ''
    Say '  ① 跑 Windows 内存诊断 —— 下面这条会弹窗，选「立即重新启动并检查问题」；'
    Say '     重启后会进蓝底测试界面，**按 F1 选「扩展」**（默认的「标准」测不出偶发错）。'
    Say ('     本机 {0} GB，扩展测试通常要几小时，让它跑完，别中途关机。' -f $totalGb)
    Say ''
    Say '         mdsched.exe'
    Say ''
    if ($dimms.Count -ge 2) {
        Say ('  ② 拔插法（本机有 {0} 条内存，可以用这个办法定位到具体哪一条）：' -f $dimms.Count)
        Say '     关机断电，只留一条开机用一段时间。哪条单独装着会崩，就是哪条。'
        Say '     内存条多的时候，这一步比软件测试更省时间。'
    } elseif ($dimms.Count -eq 1) {
        Say '  ② 本机只有 1 条内存 ⇒ **拔插法用不了**，只能替换法：'
        Say '     借一条已知好的内存换上跑一段；原条留着，换下来的表现就是判据。'
        Say '     顺便把原条拔下来用橡皮擦一下金手指、换个插槽再插回去（接触不良也会这样）。'
    } else {
        Say '  ② 拔插法 / 替换法：内存条在 2 条以上时可以拔到只剩一条定位；'
        Say '     只有 1 条时只能借一条替换。'
    }
    Say ''
    Say '  ③ 软件测不出但仍在崩，就查转储里的具体模块（把 MEMORY.DMP 拖进 WinDbg）：'
    Say ''
    Say '         .symfix; .reload; !analyze -v'
    Say ''
    Say '     看 MODULE_NAME 与 IMAGE_NAME 指到哪个驱动 —— 若每次都不一样，'
    Say '     更说明是底层在随机损坏，而不是某个驱动有 bug。'
    Say ''
    Say '  ④ 顺手扫一下磁盘（0x7A 这类「换页读入失败」也可能来自磁盘或数据线）：'
    Say ''
    Say '         chkdsk C: /scan'
    Say ''
    if ($anyOverclocked) {
        Say '  ⑤ 本机内存跑在超过 DDR4 原生上限的频率上（第 5 节）⇒ 属于 XMP/超频档。'
        Say '     先进 BIOS 把它关掉、让内存跑默认档再试 —— 低端主板上跑高频档不稳，'
        Say '     是很常见的诱因。'
    } else {
        Say '  ⑤ 本机内存跑在 DDR4 原生频率内（第 5 节），**没有**超频因素 ——'
        Say '     所以不必往 XMP 上查，重点放在内存条本身、插槽接触、以及主板供电。'
    }
    Say ''
}
finally {
    try {
        $utf8Bom = New-Object System.Text.UTF8Encoding($true)
        [System.IO.File]::WriteAllText($report, $script:Log.ToString(), $utf8Bom)
        Write-Host ''
        Write-Host ('报告已写入： ' + $report)
    } catch {
        Write-Host ''
        Write-Host ('报告写入失败： ' + $_.Exception.Message)
    }
}
