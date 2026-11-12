@echo off
rem Build the GUI. ASCII only on purpose: cmd.exe reads this file as CP936 and
rem non-ASCII comments get executed as garbage commands.
setlocal
cd /d "%~dp0.."

where dotnet >nul 2>nul
if errorlevel 1 (
    echo [!] .NET SDK not found.
    echo     Install .NET 9 SDK: https://dotnet.microsoft.com/download
    exit /b 1
)

echo Building HermesFixGui ... framework-dependent, single file
dotnet publish gui\HermesFix.csproj -c Release -r win-x64 --self-contained false ^
    -p:PublishSingleFile=true -o publish\fx --nologo
if errorlevel 1 exit /b 1

if not exist bin mkdir bin
copy /y publish\fx\HermesFixGui.exe bin\HermesFixGui.exe >nul
echo.
echo Done: bin\HermesFixGui.exe  ^(needs .NET 9 Desktop Runtime^)
echo.
echo For a standalone build that needs no runtime:
echo   dotnet publish gui\HermesFix.csproj -c Release -r win-x64 --self-contained true ^
echo       -p:PublishSingleFile=true -p:EnableCompressionInSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true -o publish\standalone
