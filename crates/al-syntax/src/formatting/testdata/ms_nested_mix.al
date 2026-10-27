codeunit 50100 "Probe Thirteen"
{
    procedure A()
    begin
        if A then begin
            X := 1;
        end else if B then begin
            X := 2;
        end else
            X := 3;
        if A then
            if B then begin
                X := 1;
            end else
                X := 2
        else
            X := 3;
        for i := 1 to 3 do
            if A then
                X := 1
            else
                X := 2;
        case X of
            1:
                if A then
                    if B then
                        Y := 1
                    else
                        Y := 2
                else
                    Y := 3;
            (A) and (B):
                begin
                    Y := 4;
                end;
            Foo::Bar.AsInteger():
                begin
                    if A then
                        Y := 1
                    else
                        Y := 2;
                end;
        end;
        if (A = 1) or
        (B = 2) then
            if (C = 1) and
            (D = 2) then begin
                X := 1;
            end else
                X := 2;
        Foo.Bar()
            .Baz(1)
            .Qux(2);
        X := A +
            B +
            C;
        if A then
            Foo.Bar()
            .Baz()
        else
            Foo.Bar()
            .Baz();
    end;
}

