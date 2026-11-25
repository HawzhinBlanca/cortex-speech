' Launches powershell.exe completely hidden (SW_HIDE = 0) so Interactive scheduled tasks never flash a console window.
Option Explicit

Function QuoteWin32Arg(ByVal s)
    Dim i, ch, bsCount, result
    If Len(s) = 0 Then
        QuoteWin32Arg = """"""
        Exit Function
    End If
    If InStr(s, " ") = 0 And InStr(s, vbTab) = 0 And InStr(s, """") = 0 Then
        QuoteWin32Arg = s
        Exit Function
    End If
    result = """"
    bsCount = 0
    For i = 1 To Len(s)
        ch = Mid(s, i, 1)
        If ch = "\" Then
            bsCount = bsCount + 1
        ElseIf ch = """" Then
            result = result & String(bsCount * 2 + 1, "\") & """"
            bsCount = 0
        Else
            If bsCount > 0 Then
                result = result & String(bsCount, "\")
                bsCount = 0
            End If
            result = result & ch
        End If
    Next
    If bsCount > 0 Then result = result & String(bsCount * 2, "\")
    QuoteWin32Arg = result & """"
End Function

If WScript.Arguments.Count = 0 Then WScript.Quit 2

Dim shell, idx, cmd
Set shell = CreateObject("WScript.Shell")
cmd = "powershell.exe"
For idx = 0 To WScript.Arguments.Count - 1
    cmd = cmd & " " & QuoteWin32Arg(WScript.Arguments(idx))
Next

WScript.Quit shell.Run(cmd, 0, True)
