// Test: Simple Option type (currently working)
codeunit 50100 "Simple Option Test"
{
    procedure GetStatus(): Integer
    begin
        exit(0);
    end;
    
    procedure GetChoice(Value: Integer) Result: Integer
    begin
        Result := Value;
    end;
}
