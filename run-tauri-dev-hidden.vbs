Option Explicit

Dim shell
Dim fso
Dim scriptDir
Dim command
Dim logPath

Set shell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")

scriptDir = fso.GetParentFolderName(WScript.ScriptFullName)
logPath = scriptDir & "\dev-tauri.log"

command = "cmd /c cd /d """ & scriptDir & """ && call run-tauri-dev.cmd > """ & logPath & """ 2>&1"
shell.Run command, 0, False
