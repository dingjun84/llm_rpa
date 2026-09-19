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
rem ============================================================

setlocal
set "ROOT=%~dp0"
set "EXE=%ROOT%target\debug\desktop.exe"
set "OCR=%ROOT%target\debug\winocr.exe"

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
echo [run] 工作目录： %CD%
echo [run] 数据目录： %CD%\data
echo [run] 界面    ： %EXE%
echo [run] OCR 程序： %OCR%
start "" "%EXE%"
popd
endlocal
