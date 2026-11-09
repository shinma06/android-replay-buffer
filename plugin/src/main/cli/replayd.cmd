@echo off
call "%~dp0replay.cmd" daemon %*
exit /b %ERRORLEVEL%
