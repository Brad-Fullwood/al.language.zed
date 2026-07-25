// Invalid: bare Option type is fine, but missing `end` must still fail
codeunit 50200 "Bad Option Bare"
{
    procedure GetStep(): Option
    begin
        exit(0);
    // Missing: end;
}
