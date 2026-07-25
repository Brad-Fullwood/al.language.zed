// Invalid: Basic codeunit structure but missing end for begin block
codeunit 50200 "Bad Basic Codeunit"
{
    procedure Foo(): Integer
    begin
        if true then
            exit(1);
    // Missing: end;
}
