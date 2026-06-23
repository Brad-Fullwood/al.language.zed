codeunit 50100 "Test Codeunit"
{
    procedure HelloWorld()
    begin
        Message('Hello, World!');
    end;

    procedure Add(a: Integer; b: Integer): Integer
    begin
        exit(a + b);
    end;

    local procedure InternalHelper()
    var
        Counter: Integer;
    begin
        Counter := 0;
        Counter += 1;
    end;
}
