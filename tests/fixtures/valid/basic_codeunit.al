codeunit 50110 Valid_Basic
{
    procedure Add(a: Integer; b: Integer): Integer;
    var
        c: Integer;
    begin
        c := a + b;
        if c > 0 then
            exit(c)
        else
            exit(0);
    end;
}

