// Test: Option with quoted members including spaces (TARGET - currently failing)
codeunit 50102 "Option Quoted Test"
{
    procedure GetPermission(PermissionType: Option Include,Exclude): Option " ",Yes,Indirect
    begin
        exit(1);
    end;
    
    procedure GetConfidence() Conf: Option " ",Low,Medium,High
    begin
        Conf := 2;
    end;
}
