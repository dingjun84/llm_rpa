@echo off
chcp 65001 >nul
rem ============================================================
rem  启动桌面程序，并把「数据目录」固定在仓库根下的 data\
rem
rem  为什么需要这个脚本：
rem    数据目录是「程序运行当前路径」下的 data\。双击
rem    target\debug\desktop.exe 时，那个"当前路径"就是 target\debug\，
rem    于是配置、图标库、任务日志、证据图全落在构建产物里 ——
rem    一次 cargo clean 就全没了（标定要重做一遍）。
rem    这个脚本先切到仓库根，再启动，数据就落在 <仓库>\data\。
rem
rem  想换个地方放数据：把整个仓库拷过去，或者改这里的 ROOT。
rem
rem  两个可执行文件都要在，缺一个就在这儿拦下来：
rem    desktop.exe —— 界面本体
rem    winocr.exe  —— 本地 OCR 程序（独立的 workspace 成员 tools/winocr）
rem
rem  为什么必须在这儿查 winocr：它是**独立进程**，任务跑到第一次识别时才去
rem  启动它。缺了的话界面照常打开、任务也点得动，一直到 SearchingContact
rem  才报「系统找不到指定的文件」——那时人已经盯着屏幕等半天了。
rem
rem  ★ 启动之后要**等一会儿再确认它还在**，理由见下面「为什么要确认」那段。
rem ============================================================

setlocal
set "ROOT=%~dp0"
set "EXE=%ROOT%target\debug\desktop.exe"
set "OCR=%ROOT%target\debug\winocr.exe"
set "LOG=%ROOT%data\startup.log"

set "MISSING="
if not exist "%EXE%" set "MISSING=%MISSING% desktop.exe"
if not exist "%OCR%" set "MISSING=%MISSING% winocr.exe"

if defined MISSING (
  echo [run] 缺少：%MISSING%
  echo.
  echo [run] 两个都要建。命令**分行**跑，别连成一行：
  echo [run]
  echo [run]   cargo build -p winocr
  echo [run]   cargo build -p desktop --features custom-protocol
  echo.
  echo [run] 注意：winocr 是独立的 workspace 成员，
  echo [run]       cargo build -p desktop  不会建它；
  echo [run]       cargo test --workspace  也只在 target\debug\deps\ 下
  echo [run]       留一个带哈希的副本，不会生成 target\debug\winocr.exe。
  echo.
  echo [run] 若上面报「拒绝访问 / os error 5」，说明程序还开着：
  echo [run] 先关掉窗口再构建（Windows 不允许覆盖正在运行的 exe）。
  pause
  exit /b 1
)

pushd "%ROOT%"

rem 上一次的启动日志先删掉：它只描述「这一次」启动，留着旧的会误导排查。
if exist "%LOG%" del /q "%LOG%" >nul 2>&1

echo [run] 工作目录： %CD%
echo [run] 数据目录： %CD%\data
echo [run] 界面    ： %EXE%
echo [run] OCR 程序： %OCR%
echo.

start "" "%EXE%"

rem ── 为什么要确认 ────────────────────────────────────────────
rem  启动期的失败（数据目录建不出来、状态初始化失败、WebView2 起不来）
rem  全都发生在**窗口出现之前**。那种情况下程序会立刻退出，而这个脚本
rem  自己也是立刻结束的 —— 两件事叠在一起，操作者看到的就只有「窗口闪了一下」，
rem  没有任何文字可看。所以这里等 4 秒、确认进程还在，并把它的启动日志打出来。
rem
rem  用 ping 而不是 timeout 当延时：timeout 在标准输入被重定向时会直接报错。
ping -n 5 127.0.0.1 >nul 2>&1

tasklist /FI "IMAGENAME eq desktop.exe" 2>nul | find /I "desktop.exe" >nul
if errorlevel 1 goto startup_failed

echo [run] 已启动。关掉本窗口不影响它。
popd
endlocal
exit /b 0

:startup_failed
echo [run] ============================================================
echo [run] 启动失败：desktop.exe 没起来，或者起来之后立刻退出了。
echo [run] ============================================================
echo.

if exist "%LOG%" (
  echo [run] 它自己的启动日志：%LOG%
  echo [run] ------------------------------------------------------------
  type "%LOG%"
  echo [run] ------------------------------------------------------------
  echo.
  echo [run] 日志里最后一条成功记录，就是它走到的地方；再往后那条就是失败原因。
) else (
  echo [run] 连 %LOG% 都没生成。
  echo [run] 说明它在写第一行日志之前就没了 —— 那多半不是本程序自己的问题，
  echo [run] 而是进程被系统或安全软件拦下了，或者 exe 本身有问题。
  echo [run] 可以先双击 %EXE% 试一次，看是不是同样结果。
)

echo.
echo [run] 把上面这些内容发给开发者即可定位。
pause
popd
endlocal
exit /b 1
