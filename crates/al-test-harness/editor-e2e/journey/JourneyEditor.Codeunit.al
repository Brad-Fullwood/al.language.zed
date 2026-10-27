codeunit 50147 "Journey Editor"
{
    procedure AddNumbers(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    procedure UseIt(): Integer
    var
        Total: Integer;
    begin
        Total := AddNumbers(1, 2);
        exit(Total);
    end;

    procedure Messy()
    var
    Count: Integer;
    begin
    Count := 1;
            if Count > 0 then
        Count := 2;
    end;
}
