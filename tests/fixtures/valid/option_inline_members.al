// Test: Option with inline members (TARGET - currently failing)
codeunit 50101 "Option Inline Test"
{
    procedure GetDirection() Result: Option Inbound,Outbound
    begin
        Result := 0;
    end;
    
    procedure GetStatus(): Option Active,Inactive,Pending
    begin
        exit(0);
    end;
}
