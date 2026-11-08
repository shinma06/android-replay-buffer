@echo off
setlocal
chcp 65001 >nul
set "plugin_dir=%~dp0.."
if defined REPLAY_STUDIO_HOME (set "studio_home=%REPLAY_STUDIO_HOME%") else (set "studio_home=%ProgramFiles%\Android\Android Studio")
set "java=%studio_home%\jbr\bin\java.exe"
if not exist "%java%" goto missing
if not exist "%studio_home%\lib\util-8.jar" goto missing
"%java%" -cp "%plugin_dir%\lib\*;%studio_home%\lib\util-8.jar" io.github.shinma06.replaybuffer.cli.ReplayCli %*
exit /b %ERRORLEVEL%
:missing
>&2 echo 対応するAndroid Studioの導入先をREPLAY_STUDIO_HOMEで指定してください。
exit /b 1
