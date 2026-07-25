// Invalid: Option with inline members is fine, but unbalanced begin/end must still fail
codeunit 50201 "Bad Option Inline"
{
    procedure GetDir() Result: Option Inbound,Outbound
    begin
        Result := 0;
    // Missing: end;
}
