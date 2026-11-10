@echo off
setlocal
chcp 65001 >nul
set "plugin_dir=%~dp0.."
rem REPLAY_STUDIO_HOME > home recorded by the last started IDE (if usable) > default install location.
set "studio_home="
if defined REPLAY_STUDIO_HOME set "studio_home=%REPLAY_STUDIO_HOME%"
if defined studio_home goto resolved
set "recorded_file=%USERPROFILE%\.android-replay-buffer-cli\studio-home"
if not defined USERPROFILE goto default_home
if not exist "%recorded_file%" goto default_home
set "recorded="
set /p recorded=<"%recorded_file%"
if not defined recorded goto default_home
if not exist "%recorded%\jbr\bin\java.exe" goto default_home
if not exist "%recorded%\lib\util-8.jar" goto default_home
set "studio_home=%recorded%"
goto resolved
:default_home
set "studio_home=%ProgramFiles%\Android\Android Studio"
:resolved
set "java=%studio_home%\jbr\bin\java.exe"
if not exist "%java%" goto missing
if not exist "%studio_home%\lib\util-8.jar" goto missing
set "jna_arch=amd64"
if /i "%PROCESSOR_ARCHITECTURE%"=="ARM64" set "jna_arch=aarch64"
"%java%" "-Djna.boot.library.path=%studio_home%\lib\jna\%jna_arch%" -Djna.nosys=true -Djna.noclasspath=true --enable-native-access=ALL-UNNAMED -cp "%plugin_dir%\lib\*;%studio_home%\lib\util-8.jar" io.github.shinma06.replaybuffer.cli.ReplayCli %*
exit /b %ERRORLEVEL%
:missing
>&2 echo 対応するAndroid Studioが見つかりません。プラグインを入れたAndroid Studioでprojectを一度開くか、導入先をREPLAY_STUDIO_HOMEで指定してください。
exit /b 1
