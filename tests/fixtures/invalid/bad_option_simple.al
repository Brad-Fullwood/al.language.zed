// Invalid: Simple procedures but extra closing brace
codeunit 50206 "Bad Simple Option"
{
    procedure GetStatus(): Integer
    begin
        exit(0);
    end;
}}
