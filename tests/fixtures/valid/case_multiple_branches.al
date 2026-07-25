codeunit 50111 Valid_Case_Branches
{
    procedure Classify(value: Integer): Integer;
    begin
        case value of
            1:
                exit(10);
            2:
                exit(20);
            3:
                begin
                    value += 1;
                    exit(value);
                end;
            else
                exit(0);
        end;
    end;
}
