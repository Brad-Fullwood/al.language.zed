codeunit 50100 "Probe Three"
{
    procedure A()
    begin
        if A then
            if B then
                X := 1
            else
                X := 2
        else
            X := 3;
        if A then begin
            X := 1;
        end else
            X := 2;
        if A then
            X := 1
        else if B then
            X := 2
        else
            X := 3;
        if A then begin
            X := 1;
        end else
            if B then
                X := 2
            else
                X := 3;
        if A then begin
        end else
            for i := 1 to 2 do
                X := 3;
    end;
}

