// Invalid: Option with quoted members is fine, but extra closing brace must still fail
codeunit 50202 "Bad Option Quoted"
{
    procedure GetPerm(PermType: Option Include,Exclude): Option " ",Yes,Indirect
    begin
        exit(1);
    end;
}}
