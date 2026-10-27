codeunit 50100 "Probe One"
{
    procedure A()
    var
        X: Integer;
    begin
#pragma warning disable AA0001
        X := 1;
        if X = 1 then begin
#pragma warning disable AA0002
            X := 2;
#pragma warning restore AA0002
        end;
#if CLEAN20
        X := 3;
#else
        X := 4;
#endif
        #region Foo
        X := 5;
        #endregion
    end;
#pragma warning restore AA0001
}

